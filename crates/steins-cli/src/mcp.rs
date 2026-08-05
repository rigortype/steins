//! `steins mcp` — the transform loop as an MCP tool surface (ADR-0010/0020,
//! issue #117).
//!
//! ADR-0010 names the interaction model this serves: an agent refactors
//! conversationally through **dry-run → diff → approve → apply**, with a
//! completeness oracle in every response. The command line already speaks that
//! loop (`transform` dry-runs, `--apply` writes); this module gives an agent
//! the same loop as structured tool calls, over the same code. Nothing here
//! re-derives a fact: the plan, the refusals, the oracle counts and the
//! post-check all come from [`crate::plan_transform_run`], the function
//! `steins transform` itself calls, and a finding is spelled by
//! [`crate::finding_json`], the same one `check --format json` prints.
//!
//! # Approve is a step, not a flag
//!
//! `plan_transform` and `apply_plan` are two tool calls, and there is no third
//! that does both. The agent must show a diff and be told to go ahead before
//! anything is written — that pause *is* the model, so it is spelled in the
//! tool surface rather than left to a client's good manners.
//!
//! # A plan handle lives in one process
//!
//! `plan_transform` returns a `plan_handle`; `apply_plan` takes one. The handle
//! is minted with this process's identity ([`Session::stamp`]) and the plan
//! itself is held in this process's memory — there is no daemon, no on-disk
//! plan store, and no way to hand a handle to a later run. A handle from
//! another process, from a restarted server, or from a plan already applied is
//! a **named error**, never a write: applying spans that were computed against
//! a tree nobody re-verified is precisely the stale write this design refuses.
//! On top of that, `apply_plan` re-reads every target and refuses if the bytes
//! moved since planning, then re-runs the ADR-0034 post-check before writing.
//!
//! # Read-only by construction
//!
//! Tool handlers come in exactly two shapes ([`Handler`]), and the shape is the
//! guarantee. A `Read` handler is handed `&Session` and answers; a `Write`
//! handler is handed `&mut Session`. There is exactly one `Write` in [`TOOLS`]
//! (a test pins that), and the module's only `std::fs::write` calls are inside
//! it. A `Read` handler cannot even record its own plan handle — it returns the
//! plan in a [`Reply`] and the dispatcher stores it — so "this tool does not
//! touch the tree" is something the compiler and the table say, not a comment.
//!
//! # Transport, and why no MCP SDK
//!
//! MCP's stdio transport is JSON-RPC 2.0 messages delimited by newlines — the
//! same wire family the PHP sidecar already speaks (ADR-0024), and the reason
//! that ADR chose it. `serde_json` plus the loop below covers `initialize`,
//! `tools/list` and `tools/call` in a few dozen lines with no new dependency,
//! no async runtime, and nothing new for the licenses gate (ADR-0025) to weigh.
//! An SDK would buy transports and protocol surface this server does not use.

use std::collections::HashMap;
use std::io::BufRead;
use std::process::ExitCode;

use serde_json::{Value, json};
use steins_edit::{CompletenessOracle, EditPlan, unified_diff};
use steins_infer::{Diagnostic, SidecarFolder, check_project_with_runtime};

use crate::{TransformKind, profile};

/// The MCP revision this server implements. `initialize` echoes the client's
/// requested version when it is one of [`SPOKEN_VERSIONS`], which is what a
/// client that speaks an older revision needs to hear.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Revisions whose `initialize` / `tools/list` / `tools/call` shapes this
/// server answers identically.
const SPOKEN_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

// JSON-RPC 2.0 error codes. Protocol-level failures use these; a *tool* failure
// is never one of them — it comes back as a result with `isError` set and a
// named reason in `structuredContent`, so an agent can read and act on it.
const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// `steins mcp` — serve the tool surface over stdio until the client closes it.
///
/// Takes no arguments: what to analyze is a tool argument, not a process one,
/// because one server answers about many paths over its lifetime.
pub(crate) fn run_mcp(args: &[String]) -> ExitCode {
    if let Some(arg) = args.first() {
        errln!("steins: mcp takes no arguments (got `{arg}`); usage: steins mcp");
        return ExitCode::from(2);
    }

    let mut session = Session::new();
    // stderr is the log channel: stdout carries the protocol and nothing else.
    errln!(
        "steins: mcp serving on stdio (protocol {PROTOCOL_VERSION}, session {}); plan handles are valid only in this process",
        session.stamp
    );

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                errln!("steins: mcp: cannot read stdin: {e}");
                return ExitCode::FAILURE;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(response) = handle_message(&mut session, trimmed) else { continue };
        match serde_json::to_string(&response) {
            Ok(text) => outln!("{text}"),
            Err(e) => errln!("steins: mcp: cannot serialize a response: {e}"),
        }
    }
    ExitCode::SUCCESS
}

/// Answer one JSON-RPC message, or `None` when there is nothing to answer — a
/// notification (`notifications/initialized` and friends) carries no id, and a
/// message with an id but no method is a response to a request this server
/// never sends.
fn handle_message(session: &mut Session, line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error_response(Value::Null, PARSE_ERROR, format!("not JSON: {e}"), None));
        }
    };
    let id = message.get("id").cloned().filter(|v| !v.is_null())?;
    let method = message.get("method").and_then(Value::as_str)?.to_owned();

    Some(match method.as_str() {
        "initialize" => result_response(id, initialize_result(&message)),
        "ping" => result_response(id, json!({})),
        "tools/list" => result_response(id, json!({ "tools": tool_descriptors() })),
        "tools/call" => tools_call(session, id, &message),
        other => error_response(
            id,
            METHOD_NOT_FOUND,
            format!("unknown method `{other}`"),
            Some(json!({ "supported": ["initialize", "ping", "tools/list", "tools/call"] })),
        ),
    })
}

/// The `initialize` result: what this server is and what it offers. The
/// instructions field is the one place an agent is told the two rules it cannot
/// infer from the schemas — approve is a separate call, and a handle dies with
/// the process.
fn initialize_result(message: &Value) -> Value {
    let requested = message
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .filter(|v| SPOKEN_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": requested,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "steins",
            "title": "Steins",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": "Refactor through the loop, not around it: `plan_transform` for a dry run (per-site diffs, the enumerated/transformed/refused oracle, and a named reason for every refusal), then show the diff, then `apply_plan` with the handle the plan returned once the human approves. There is no plan-and-apply call. A plan handle is memory in this server process: it is consumed by the apply that uses it, and a handle from an earlier run or another connection is an error rather than a write. `check` reports findings, each carrying its `fix` payload where a mechanical remedy exists.",
    })
}

/// Route a `tools/call`. An unknown tool name is a protocol-level invalid-params
/// error (the client asked for something that is not on the menu); everything a
/// *tool* refuses comes back as a result with `isError` and a named reason.
fn tools_call(session: &mut Session, id: Value, message: &Value) -> Value {
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error_response(id, INVALID_PARAMS, "tools/call requires a `name`", None);
    };
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    match session.call_tool(name, &args) {
        None => error_response(
            id,
            INVALID_PARAMS,
            format!("unknown tool `{name}`"),
            Some(json!({ "available": TOOLS.iter().map(|t| t.name).collect::<Vec<_>>() })),
        ),
        Some(Ok(value)) => result_response(id, tool_result(value, false)),
        Some(Err(e)) => result_response(id, tool_result(e.into_value(), true)),
    }
}

/// A tool result. The document is carried twice on purpose: `structuredContent`
/// for a client that reads JSON, and the same document pretty-printed as text
/// content for one that does not.
fn tool_result(value: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("{{\"error\":{{\"reason\":\"serialize-failed\",\"detail\":\"{e}\"}}}}"));
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": is_error,
    })
}

fn result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message.into() });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

// ---------------------------------------------------------------------------
// The session: plan handles, and the one place a plan is remembered.
// ---------------------------------------------------------------------------

/// A plan waiting for approval: everything `apply_plan` needs to re-verify and
/// write, and nothing that outlives the process holding it.
#[derive(Clone, Debug)]
struct StoredPlan {
    kind: TransformKind,
    plan: EditPlan,
    /// The exact bytes each analyzed file had when the plan was computed. Apply
    /// compares the edited files against these before splicing: a byte span is
    /// only meaningful against the text it was measured in.
    texts: HashMap<String, String>,
    /// The paths the plan was made over — re-analyzed at apply time so the
    /// post-check measures the project, not a snapshot.
    paths: Vec<String>,
    oracle: CompletenessOracle,
}

/// One connection's state. A [`Session`] is created by [`run_mcp`] and dropped
/// when the client disconnects, which is the whole lifetime of every plan
/// handle it ever mints.
struct Session {
    /// This process's identity, stamped into every handle: pid plus a start
    /// nonce, so a handle cannot survive a restart even onto a recycled pid.
    stamp: String,
    /// The next handle's sequence number.
    next: u64,
    plans: HashMap<String, StoredPlan>,
}

impl Session {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        Self { stamp: format!("{}-{nonce}", std::process::id()), next: 1, plans: HashMap::new() }
    }

    /// Dispatch a tool call, returning `None` when no tool has that name.
    ///
    /// The two handler shapes meet here and nowhere else: a `Read` handler gets
    /// `&Session` — it cannot store anything, so it cannot be the tool that
    /// mutates — and the plan it produces is remembered by [`Self::remember`]
    /// on the way out.
    fn call_tool(&mut self, name: &str, args: &Value) -> Option<Result<Value, ToolError>> {
        let tool = TOOLS.iter().find(|t| t.name == name)?;
        let reply = match tool.handler {
            Handler::Read(f) => f(self, args),
            Handler::Write(f) => f(self, args),
        };
        Some(reply.map(|reply| self.remember(reply)))
    }

    /// Mint a handle for a plan a handler produced and stamp it into the
    /// response document.
    fn remember(&mut self, reply: Reply) -> Value {
        let Reply { mut value, plan } = reply;
        if let Some(plan) = plan {
            let handle = format!("steins-plan-{}-{}", self.stamp, self.next);
            self.next += 1;
            value["plan_handle"] = Value::String(handle.clone());
            self.plans.insert(handle, plan);
        }
        value
    }

    /// Resolve a handle, distinguishing the three ways one can fail to name a
    /// plan this process is holding. Each is a named refusal an agent can act
    /// on; none of them can become a write.
    fn plan_for(&self, handle: &str) -> Result<&StoredPlan, ToolError> {
        let stamp = handle
            .strip_prefix("steins-plan-")
            .and_then(|rest| rest.rsplit_once('-'))
            .map(|(stamp, _seq)| stamp)
            .ok_or_else(|| {
                ToolError::new(
                    "plan-handle-malformed",
                    format!(
                        "`{handle}` is not a plan handle — pass back the `plan_handle` string a plan_transform call returned"
                    ),
                )
            })?;
        if stamp != self.stamp {
            return Err(ToolError::new(
                "plan-handle-foreign-process",
                format!(
                    "plan handle `{handle}` was minted by a different steins process (this server is session {}). A plan is memory in the process that produced it: there is no daemon and no plan store on disk, so a handle from an earlier `steins mcp` run, from a restarted server, or from another connection cannot be applied here — its byte spans were measured against a tree this process never verified. Call plan_transform again on this connection and apply the handle it returns.",
                    self.stamp
                ),
            )
            .with(json!({ "session": self.stamp })));
        }
        self.plans.get(handle).ok_or_else(|| {
            ToolError::new(
                "plan-handle-unknown",
                format!(
                    "plan handle `{handle}` is not open on this connection — apply_plan consumes the handle it applies, so this one was either already applied or never minted. Call plan_transform again to get a fresh plan and diff."
                ),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// The tool table.
// ---------------------------------------------------------------------------

/// What a tool handler returns: the response document, plus — for
/// `plan_transform` — the plan the dispatcher should remember. A handler never
/// stores anything itself; see [`Session::call_tool`].
struct Reply {
    value: Value,
    plan: Option<StoredPlan>,
}

impl Reply {
    fn plain(value: Value) -> Self {
        Self { value, plan: None }
    }

    fn with_plan(value: Value, plan: StoredPlan) -> Self {
        Self { value, plan: Some(plan) }
    }
}

/// A tool failure the agent is meant to read: a stable machine-readable
/// `reason` and a human `detail` — ADR-0034's Refusal discipline, applied to
/// the tool surface.
struct ToolError {
    reason: &'static str,
    detail: String,
    extra: Value,
}

impl ToolError {
    fn new(reason: &'static str, detail: impl Into<String>) -> Self {
        Self { reason, detail: detail.into(), extra: Value::Null }
    }

    /// Attach further named facts (the diagnostics a post-check would surface,
    /// the tools that do exist, …).
    fn with(mut self, extra: Value) -> Self {
        self.extra = extra;
        self
    }

    fn into_value(self) -> Value {
        let mut error = json!({ "reason": self.reason, "detail": self.detail });
        if let Some(extra) = self.extra.as_object() {
            for (k, v) in extra {
                error[k.as_str()] = v.clone();
            }
        }
        json!({ "error": error })
    }
}

/// The two handler shapes — and the read-only guarantee (see the module docs).
enum Handler {
    /// Reads the project and answers. Handed `&Session`, so it can neither
    /// remember a plan nor consume one.
    Read(fn(&Session, &Value) -> Result<Reply, ToolError>),
    /// The one tool that writes to the tree.
    Write(fn(&mut Session, &Value) -> Result<Reply, ToolError>),
}

struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    schema: fn() -> Value,
    handler: Handler,
}

/// The surface (ADR-0010): enumerate transforms, plan one, approve it by
/// applying its handle, and check.
static TOOLS: &[Tool] = &[
    Tool {
        name: "list_transforms",
        title: "List transforms",
        description: "Enumerate the transforms this build can plan, with what each rewrites and which surface its post-check is measured against. Read-only.",
        schema: || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        handler: Handler::Read(tool_list_transforms),
    },
    Tool {
        name: "plan_transform",
        title: "Plan a transform (dry run)",
        description: "Dry-run a transform over paths: returns the atomic edit plan, a unified diff per edited file, the completeness oracle (every enumerated site accounted for as transformed or refused), a named reason for each refusal, any project-global dynamic-code obstacles, and the zero-new-diagnostics post-check verdict. Writes nothing. Returns a `plan_handle` when there is something to apply; show the diff and get approval, then call apply_plan with it.",
        schema: || json!({
            "type": "object",
            "properties": {
                "transform": {
                    "type": "string",
                    "description": "Which transform to plan.",
                    "enum": TransformKind::ALL.map(TransformKind::id),
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Files or directories. Directories are walked for `.php` files, and every path in one call forms a single project, so cross-file calls and class chains resolve.",
                },
                "config": {
                    "type": "string",
                    "description": "Path to a steins.toml for `[transform.vouch]` / `[transform.partitions]`. Defaults to ./steins.toml when present.",
                },
            },
            "required": ["transform", "paths"],
            "additionalProperties": false,
        }),
        handler: Handler::Read(tool_plan_transform),
    },
    Tool {
        name: "apply_plan",
        title: "Apply an approved plan",
        description: "Write a previously planned transform — the approve step of the loop, and the only tool that modifies the tree. Takes the `plan_handle` from a plan_transform call in this same session; re-reads every target to confirm the bytes have not moved since planning, re-runs the post-check, and only then writes, reporting the files written. A handle from another process or an already-applied one is a named error, never a write.",
        schema: || json!({
            "type": "object",
            "properties": {
                "plan_handle": {
                    "type": "string",
                    "description": "The handle returned by plan_transform on this connection.",
                },
            },
            "required": ["plan_handle"],
            "additionalProperties": false,
        }),
        handler: Handler::Write(tool_apply_plan),
    },
    Tool {
        name: "check",
        title: "Check paths",
        description: "Analyze paths and return the findings `steins check` would report, each with its `fix` payload where a mechanical remedy exists (byte-span edits an agent or editor can apply directly). Vendor filtering, the profile surface and inline `@steins-ignore` apply as on the command line; the baseline file is not consulted, because an agent asking what is true about the code should not be answered through a CI ratchet. Read-only.",
        schema: || json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Files or directories, analyzed as one project.",
                },
                "profile": {
                    "type": "string",
                    "description": "Display profile (ADR-0050). Defaults to `[check] profile` in steins.toml, else the built-in default.",
                },
                "no_php": {
                    "type": "boolean",
                    "description": "Analyze the sound subset only — never spawn the PHP sidecar for constant folding.",
                },
                "vendor_diagnostics": {
                    "type": "boolean",
                    "description": "Report findings inside vendor directories too (off by default).",
                },
            },
            "required": ["paths"],
            "additionalProperties": false,
        }),
        handler: Handler::Read(tool_check),
    },
];

fn tool_descriptors() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "inputSchema": (t.schema)(),
                // The MCP hint an agent uses to decide what needs confirming.
                // Exactly one tool in this surface is not read-only.
                "annotations": {
                    "title": t.title,
                    "readOnlyHint": matches!(t.handler, Handler::Read(_)),
                },
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The tools.
// ---------------------------------------------------------------------------

fn tool_list_transforms(_session: &Session, _args: &Value) -> Result<Reply, ToolError> {
    let transforms: Vec<Value> = TransformKind::ALL
        .iter()
        .map(|k| {
            json!({
                "id": k.id(),
                "summary": k.summary(),
                "oracle_verb": k.action(),
                "post_check_surface": k.post_check_surface().name(),
            })
        })
        .collect();
    Ok(Reply::plain(json!({ "transforms": transforms })))
}

/// The dry-run half of the loop. Everything it reports comes from
/// [`crate::plan_transform_run`] — the same call `steins transform` makes.
fn tool_plan_transform(_session: &Session, args: &Value) -> Result<Reply, ToolError> {
    let id = string_arg(args, "transform")?;
    let kind = TransformKind::from_id(&id).ok_or_else(|| {
        ToolError::new("unknown-transform", format!("no transform named `{id}`")).with(
            json!({ "available": TransformKind::ALL.map(TransformKind::id) }),
        )
    })?;
    let paths = paths_arg(args)?;
    let config = optional_string_arg(args, "config")?;

    let run = crate::plan_transform_run(kind, &paths, config.as_deref())
        .map_err(|e| ToolError::new("config-error", e))?;

    // The `--format json` document, plus what the command line prints as a
    // diff and what only an agent surface can carry: a handle.
    let mut value = crate::transform_json(&run.report, &run.postcheck, false);
    let diffs: Vec<Value> = run
        .report
        .plan
        .edited_paths()
        .iter()
        .filter_map(|path| {
            let original = run.texts.get(*path)?;
            let updated = run.report.plan.apply_file(path, original);
            Some(json!({ "path": path, "diff": unified_diff(path, original, &updated, 3) }))
        })
        .collect();
    value["transform"] = json!(kind.id());
    value["diffs"] = json!(diffs);
    value["notices"] = json!(run.notices);
    value["post_check_surface"] = json!(kind.post_check_surface().name());
    // An empty plan has nothing to approve, so it mints no handle: the oracle
    // still reports every enumerated site and why each was refused.
    if run.report.plan.is_empty() {
        value["plan_handle"] = Value::Null;
        return Ok(Reply::plain(value));
    }
    let stored = StoredPlan {
        kind,
        plan: run.report.plan.clone(),
        texts: run.texts,
        paths,
        oracle: run.report.oracle,
    };
    Ok(Reply::with_plan(value, stored))
}

/// The approve half of the loop, and the only tool that writes.
///
/// Three gates stand between a handle and a byte on disk: the handle must name
/// a plan *this* process is holding, every target must still hold the bytes the
/// plan was computed against, and the ADR-0034 post-check must pass again on
/// the surface this transform names. Each failure is a named refusal that
/// leaves the tree exactly as it was, and the handle stays open for a retry.
fn tool_apply_plan(session: &mut Session, args: &Value) -> Result<Reply, ToolError> {
    let handle = string_arg(args, "plan_handle")?;
    let stored = session.plan_for(&handle)?.clone();

    // Gate 1: the tree must still be the tree that was planned against. A span
    // is only meaningful in the text it was measured in, so an edited file that
    // moved under us is refused rather than spliced.
    for path in stored.plan.edited_paths() {
        let planned = stored.texts.get(path).ok_or_else(|| {
            ToolError::new(
                "plan-target-unread",
                format!("the plan edits {path} but carries no source text for it"),
            )
        })?;
        let current = std::fs::read(path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .map_err(|e| {
                ToolError::new("plan-target-unreadable", format!("cannot re-read {path}: {e}"))
            })?;
        if &current != planned {
            return Err(ToolError::new(
                "tree-changed-since-plan",
                format!(
                    "{path} has changed since this plan was made, so its byte spans no longer describe the file on disk. Nothing was written. Call plan_transform again and review the new diff."
                ),
            ));
        }
    }
    // A plan that creates a file must not silently replace one. No transform
    // emits new files today; if one does, clobbering is a decision for a human.
    for new_file in &stored.plan.new_files {
        if std::path::Path::new(&new_file.path).exists() {
            return Err(ToolError::new(
                "new-file-exists",
                format!("the plan creates {} but that path already exists", new_file.path),
            ));
        }
    }

    // Gate 2: the dual-verification post-check (ADR-0034 point 3a), re-run
    // against the project as it stands now, on the surface this transform
    // names — not a cached verdict from planning time.
    let files = crate::collect_files(&stored.paths);
    let loaded = crate::load_project(&files, &stored.paths, crate::allow_list_from_disk().as_deref());
    let postcheck = crate::post_check(
        &loaded.db,
        loaded.project,
        &stored.plan,
        &loaded.texts,
        stored.kind.post_check_surface(),
    );
    if !postcheck.ok {
        let new_diagnostics: Vec<Value> =
            postcheck.new_diagnostics.iter().map(diagnostic_json).collect();
        return Err(ToolError::new(
            "postcheck-new-diagnostics",
            format!(
                "applying this plan would surface {} new diagnostic(s); nothing was written",
                postcheck.new_diagnostics.len()
            ),
        )
        .with(json!({ "new_diagnostics": new_diagnostics })));
    }

    // The write. These are the only `std::fs::write` calls in this module, and
    // they are reachable only from the surface's one `Handler::Write`.
    let mut written: Vec<String> = Vec::new();
    for path in stored.plan.edited_paths() {
        let original = stored.texts.get(path).expect("gate 1 read every edited path");
        let updated = stored.plan.apply_file(path, original);
        std::fs::write(path, &updated).map_err(|e| {
            ToolError::new(
                "write-failed",
                format!("cannot write {path}: {e} ({} file(s) already written)", written.len()),
            )
            .with(json!({ "files_written": written.clone() }))
        })?;
        written.push((*path).to_owned());
    }
    for new_file in &stored.plan.new_files {
        std::fs::write(&new_file.path, &new_file.contents).map_err(|e| {
            ToolError::new(
                "write-failed",
                format!(
                    "cannot create {}: {e} ({} file(s) already written)",
                    new_file.path,
                    written.len()
                ),
            )
            .with(json!({ "files_written": written.clone() }))
        })?;
        written.push(new_file.path.clone());
    }

    // The handle is spent: a plan describes a tree that no longer exists.
    session.plans.remove(&handle);
    Ok(Reply::plain(json!({
        "applied": true,
        "transform": stored.kind.id(),
        "files_written": written,
        "oracle": {
            "enumerated": stored.oracle.enumerated,
            "transformed": stored.oracle.transformed,
            "refused": stored.oracle.refused,
            "complete": stored.oracle.is_complete(),
        },
        "postcheck": { "ok": true, "new_diagnostics": [] },
        "plan_handle_consumed": handle,
    })))
}

/// The findings `steins check` would report, with their fix payloads.
fn tool_check(_session: &Session, args: &Value) -> Result<Reply, ToolError> {
    let paths = paths_arg(args)?;
    let no_php = bool_arg(args, "no_php")?;
    let vendor_diagnostics = bool_arg(args, "vendor_diagnostics")?;
    let requested_profile = optional_string_arg(args, "profile")?;

    // The same config read `check` performs, with the same hard-error posture:
    // a malformed steins.toml is a refusal, never a warn-and-proceed.
    let config = crate::read_steins_config().map_err(|e| ToolError::new("config-error", e))?;
    let (check_cfg, profile_tbl, runtime_cfg, plugin_allow) = match config {
        Some(c) => (c.check, c.profile, c.runtime, crate::allow_list(c.plugins)),
        None => (None, None, None, None),
    };
    let (config_profile, profile_configs) = crate::profiles_from_config(check_cfg, profile_tbl);
    let selected = requested_profile.as_deref().or(config_profile.as_deref());
    let surface: profile::Surface = profile_configs
        .resolve(selected)
        .map_err(|e| ToolError::new("unknown-profile", e.to_string()))?;

    let mut folder = if no_php { SidecarFolder::new(true) } else { SidecarFolder::enabled() };
    let files = crate::collect_files(&paths);
    let loaded = crate::load_project(&files, &paths, plugin_allow.as_deref());
    folder.set_php_target(loaded.layout.php_target().cloned());
    let (warning_handler_abort, runtime_notices) = crate::runtime_from_config(runtime_cfg);
    let findings =
        check_project_with_runtime(&loaded.db, loaded.project, &mut folder, warning_handler_abort);
    let (inline, vendor_suppressed) =
        crate::suppression_pipeline(&loaded, findings, &surface, vendor_diagnostics);

    let mut displayed = inline.kept;
    displayed.extend(inline.meta);
    displayed.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.column, a.id).cmp(&(b.path.as_str(), b.line, b.column, b.id))
    });
    let findings: Vec<Value> =
        displayed.iter().map(|d| crate::finding_json(d, &surface)).collect();

    Ok(Reply::plain(json!({
        "findings": findings,
        "profile": surface.name,
        "vendor_suppressed": vendor_suppressed,
        "suppressed": inline.suppressed,
        "sound_subset": no_php,
        "notices": runtime_notices,
    })))
}

/// A post-check diagnostic, spelled as `transform --format json` spells one.
fn diagnostic_json(d: &Diagnostic) -> Value {
    json!({ "id": d.id, "path": d.path, "line": d.line, "column": d.column, "message": d.message })
}

// ---------------------------------------------------------------------------
// Argument reading. Every failure is a named refusal, not a panic.
// ---------------------------------------------------------------------------

fn string_arg(args: &Value, key: &str) -> Result<String, ToolError> {
    match args.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => {
            Err(ToolError::new("invalid-argument", format!("`{key}` must not be empty")))
        }
        Some(_) => Err(ToolError::new("invalid-argument", format!("`{key}` must be a string"))),
        None => Err(ToolError::new("missing-argument", format!("`{key}` is required"))),
    }
}

fn optional_string_arg(args: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => string_arg(args, key).map(Some),
    }
}

fn bool_arg(args: &Value, key: &str) -> Result<bool, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(ToolError::new("invalid-argument", format!("`{key}` must be a boolean"))),
    }
}

/// The `paths` argument, held to the command line's rule (ADR-0050 §7): a path
/// that names nothing is refused up front, so a renamed directory can never
/// come back as a clean empty report.
fn paths_arg(args: &Value) -> Result<Vec<String>, ToolError> {
    let Some(Value::Array(items)) = args.get("paths") else {
        return Err(match args.get("paths") {
            None => ToolError::new("missing-argument", "`paths` is required"),
            Some(_) => ToolError::new("invalid-argument", "`paths` must be an array of strings"),
        });
    };
    if items.is_empty() {
        return Err(ToolError::new("invalid-argument", "`paths` must name at least one path"));
    }
    let mut paths = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(s) if !s.is_empty() => paths.push(s.clone()),
            _ => {
                return Err(ToolError::new(
                    "invalid-argument",
                    "every entry of `paths` must be a non-empty string",
                ));
            }
        }
    }
    let missing = crate::missing_paths(&paths);
    if !missing.is_empty() {
        let missing: Vec<String> = missing.into_iter().cloned().collect();
        return Err(ToolError::new(
            "path-does-not-exist",
            format!("no such path: {}", missing.join(", ")),
        )
        .with(json!({ "missing": missing })));
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The read-only claim, as a fact about the table rather than a comment:
    /// exactly one tool is a [`Handler::Write`], and it is `apply_plan`.
    #[test]
    fn exactly_one_tool_can_write_to_the_tree() {
        let writers: Vec<&str> = TOOLS
            .iter()
            .filter(|t| matches!(t.handler, Handler::Write(_)))
            .map(|t| t.name)
            .collect();
        assert_eq!(writers, vec!["apply_plan"]);
        // And the descriptors say so, so a client can see it before calling.
        for descriptor in tool_descriptors() {
            let name = descriptor["name"].as_str().unwrap().to_owned();
            let read_only = descriptor["annotations"]["readOnlyHint"].as_bool().unwrap();
            assert_eq!(read_only, name != "apply_plan", "readOnlyHint on {name}");
        }
    }

    /// A handle carries this process's identity, so one minted anywhere else is
    /// refused by name — the property that makes apply-after-restart an error
    /// rather than a stale write.
    #[test]
    fn a_handle_from_another_process_is_refused_by_name() {
        let mut session = Session::new();
        let value = session.remember(Reply::with_plan(
            json!({}),
            StoredPlan {
                kind: TransformKind::Promote,
                plan: EditPlan::new(),
                texts: HashMap::new(),
                paths: Vec::new(),
                oracle: CompletenessOracle::default(),
            },
        ));
        let mine = value["plan_handle"].as_str().expect("a handle was minted").to_owned();
        assert!(session.plan_for(&mine).is_ok());

        // Same shape, another process's stamp.
        let theirs = "steins-plan-1-1-1";
        let err = session.plan_for(theirs).expect_err("a foreign handle cannot resolve");
        assert_eq!(err.reason, "plan-handle-foreign-process");

        // Not a handle at all.
        let err = session.plan_for("nonsense").expect_err("a malformed handle cannot resolve");
        assert_eq!(err.reason, "plan-handle-malformed");

        // This process, but no such plan (an applied handle is removed).
        let spent = format!("steins-plan-{}-99", session.stamp);
        let err = session.plan_for(&spent).expect_err("an unknown sequence cannot resolve");
        assert_eq!(err.reason, "plan-handle-unknown");
    }

    /// Two sessions in the same process still cannot share a handle: the nonce
    /// separates them, so "valid only in the process that produced it" is not
    /// weakened by pid reuse.
    #[test]
    fn sessions_do_not_share_handles() {
        let a = Session::new();
        let b = Session::new();
        assert_ne!(a.stamp, b.stamp);
    }

    #[test]
    fn a_notification_gets_no_response() {
        let mut session = Session::new();
        let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(handle_message(&mut session, notification).is_none());
    }

    #[test]
    fn an_unknown_method_is_a_jsonrpc_error() {
        let mut session = Session::new();
        let request = r#"{"jsonrpc":"2.0","id":7,"method":"resources/list"}"#;
        let response = handle_message(&mut session, request).expect("a request is answered");
        assert_eq!(response["id"], 7);
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn malformed_json_is_answered_with_a_parse_error() {
        let mut session = Session::new();
        let response = handle_message(&mut session, "{ not json").expect("answered");
        assert_eq!(response["error"]["code"], PARSE_ERROR);
        assert_eq!(response["id"], Value::Null);
    }

    #[test]
    fn initialize_echoes_a_version_it_speaks() {
        let older = json!({ "params": { "protocolVersion": "2024-11-05" } });
        assert_eq!(initialize_result(&older)["protocolVersion"], "2024-11-05");
        let future = json!({ "params": { "protocolVersion": "2099-01-01" } });
        assert_eq!(initialize_result(&future)["protocolVersion"], PROTOCOL_VERSION);
    }
}
