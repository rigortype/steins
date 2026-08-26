//! End-to-end tests for `steins mcp` (ADR-0010/0020, issue #117): a scripted client drives
//! the real binary over stdio: list tools, plan a transform, read the diff and oracle,
//! approve by applying the handle.
//!
//! Pins the model, not the plumbing: planning never writes, apply is a separate call, a
//! foreign handle is a named error rather than a stale write, and read-only tools leave the
//! tree exactly as found.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test scrubs `GITHUB_ACTIONS`: `check`'s format auto-detection
/// (ADR-0054 §6) reads it and would else emit workflow commands instead of text.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

/// A throwaway project directory under the OS temp dir, cleaned on drop.
struct TempProject {
    dir: PathBuf,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-mcp-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap()
    }
    fn path(&self) -> &str {
        self.dir.to_str().unwrap()
    }

    /// The generation `CURRENT` names and when it was last written — `None`
    /// until a run publishes one. The stamp is how this file observes that a
    /// replay re-parsed nothing without asking the server to say so: the
    /// lifecycle keeps `CURRENT` exactly when the run parsed no file at all,
    /// and republishes (rewriting it) otherwise.
    fn generation(&self) -> Option<(String, std::time::SystemTime)> {
        let current = self.dir.join(".steins/gen/CURRENT");
        let id = std::fs::read_to_string(&current).ok()?.trim().to_owned();
        let stamp = std::fs::metadata(&current).ok()?.modified().ok()?;
        Some((id, stamp))
    }

    /// What `steins check --no-cache` reports over the same tree, as the JSON
    /// report's own `findings` array — the same per-finding spelling the tool
    /// reply carries, so the two compare directly.
    fn cold_findings(&self) -> Value {
        let out = steins_cmd()
            .current_dir(&self.dir)
            .args(["check", "--no-php", "--no-cache", "--format", "json"])
            .arg(self.path())
            .output()
            .expect("run steins check");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let doc: Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("check --format json is not JSON ({e}): {stdout}"));
        doc["findings"].clone()
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The scripted client: a real child process speaking JSON-RPC over stdio like an MCP host.
struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    /// Start a server rooted at `cwd` (where `steins.toml` is looked up) and complete the
    /// MCP handshake.
    fn start(cwd: &str) -> Self {
        let mut child = steins_cmd()
            .arg("mcp")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn steins mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut client = Client { child, stdin, stdout, next_id: 1 };

        let init = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "scripted-client", "version": "0" },
            }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "steins", "handshake: {init}");
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18", "handshake: {init}");
        assert!(init["result"]["capabilities"]["tools"].is_object(), "no tools capability: {init}");
        client.notify("notifications/initialized");
        client
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("write request");
        self.stdin.flush().expect("flush request");
    }

    /// Send a request and read its response.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        assert!(!line.trim().is_empty(), "server closed the stream on `{method}`");
        let response: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("response to `{method}` is not JSON ({e}): {line}"));
        assert_eq!(response["jsonrpc"], "2.0", "not JSON-RPC: {response}");
        assert_eq!(response["id"], id, "response id mismatch: {response}");
        response
    }

    /// A notification is one-way: nothing may come back, and the server must stay live
    /// afterwards.
    fn notify(&mut self, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    /// Call a tool, returning its structured document and whether it is an error.
    fn call(&mut self, name: &str, arguments: Value) -> (Value, bool) {
        let response = self.request("tools/call", json!({ "name": name, "arguments": arguments }));
        let result = &response["result"];
        let is_error = result["isError"].as_bool().unwrap_or(false);
        // The document is carried twice; a client may read either.
        let text = result["content"][0]["text"].as_str().expect("text content");
        let parsed: Value = serde_json::from_str(text).expect("text content is the same JSON");
        assert_eq!(parsed, result["structuredContent"], "content and structuredContent disagree");
        (result["structuredContent"].clone(), is_error)
    }

    /// A tool call that must succeed.
    fn call_ok(&mut self, name: &str, arguments: Value) -> Value {
        let (value, is_error) = self.call(name, arguments);
        assert!(!is_error, "`{name}` failed: {value}");
        value
    }

    /// A tool call that must fail, returning the named error object.
    fn call_err(&mut self, name: &str, arguments: Value) -> Value {
        let (value, is_error) = self.call(name, arguments);
        assert!(is_error, "`{name}` unexpectedly succeeded: {value}");
        value["error"].clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A promotable function beside one that must be refused: the completeness oracle's two
/// halves.
const LIB: &str = "<?php\n/** @param int $x */\nfunction f($x) { return $x; }\n/** @param int $y */\nfunction g($y) { return $y; }\n";
const MAIN: &str = "<?php\nf(1);\ng(\"nope\");\n";

/// The generation fixture: two declared functions, called wrongly from a second
/// file, so the sound subset alone reports and an edit adds a finding without
/// moving the one already there.
const DEFS: &str =
    "<?php\nfunction width(int $w): int { return $w; }\nfunction area(int $a): int { return $a; }\n";
const ONE_WRONG_CALL: &str = "<?php\nwidth(\"abc\");\n";
const TWO_WRONG_CALLS: &str = "<?php\nwidth(\"abc\");\narea(null);\n";

#[test]
fn a_scripted_client_lists_tools_and_drives_plan_then_apply() {
    let proj = TempProject::new("loop");
    proj.write("lib.php", LIB);
    proj.write("main.php", MAIN);
    let mut client = Client::start(proj.path());

    // list
    let listed = client.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["list_transforms", "plan_transform", "apply_plan", "check"]);
    for tool in tools {
        assert!(!tool["description"].as_str().unwrap().is_empty(), "described: {tool}");
        assert_eq!(tool["inputSchema"]["type"], "object", "schema: {tool}");
        let read_only = tool["annotations"]["readOnlyHint"].as_bool().unwrap();
        assert_eq!(read_only, tool["name"] != "apply_plan", "readOnlyHint: {tool}");
    }

    let transforms = client.call_ok("list_transforms", json!({}));
    let ids: Vec<&str> = transforms["transforms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"phpdoc-to-native"), "transforms: {transforms}");
    // Each transform names the surface its post-check is measured on (#115).
    let envelope = transforms["transforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "throws-envelope")
        .expect("throws-envelope listed");
    assert_eq!(envelope["post_check_surface"], "default-only");

    // plan (dry run)
    let args = json!({ "transform": "phpdoc-to-native", "paths": [proj.path()] });
    let plan = client.call_ok("plan_transform", args);

    // The oracle: every enumerated site accounted for as transformed or refused.
    assert_eq!(plan["report"]["oracle"]["enumerated"], 2, "oracle: {plan}");
    assert_eq!(plan["report"]["oracle"]["transformed"], 1, "oracle: {plan}");
    assert_eq!(plan["report"]["oracle"]["refused"], 1, "oracle: {plan}");
    let refusals = plan["report"]["refusals"].as_array().expect("refusals array");
    assert_eq!(refusals.len(), 1, "refusals: {plan}");
    assert_eq!(refusals[0]["reason"], "argument-not-proven");
    assert!(!refusals[0]["detail"].as_str().unwrap().is_empty(), "refusal detail: {plan}");
    // A per-site diff, the same one the command line prints.
    let diffs = plan["diffs"].as_array().expect("diffs array");
    assert_eq!(diffs.len(), 1, "one edited file: {plan}");
    assert!(diffs[0]["diff"].as_str().unwrap().contains("+function f(int $x)"), "diff: {plan}");
    assert_eq!(plan["postcheck"]["ok"], true, "postcheck: {plan}");
    assert_eq!(plan["applied"], false, "planning never writes: {plan}");
    assert_eq!(proj.read("lib.php"), LIB, "the dry run must not touch the tree");

    let handle = plan["plan_handle"].as_str().expect("a plan handle").to_owned();

    // apply (the approve step, a separate call)
    let applied = client.call_ok("apply_plan", json!({ "plan_handle": handle }));
    assert_eq!(applied["applied"], true, "apply: {applied}");
    assert_eq!(applied["transform"], "phpdoc-to-native");
    let written = applied["files_written"].as_array().expect("files_written");
    assert_eq!(written.len(), 1, "one file written: {applied}");
    assert!(written[0].as_str().unwrap().ends_with("lib.php"), "written: {applied}");
    assert_eq!(applied["oracle"]["complete"], true, "oracle: {applied}");

    let after = proj.read("lib.php");
    assert!(after.contains("function f(int $x)"), "not promoted on disk:\n{after}");
    assert!(after.contains("function g($y)"), "the refused site is untouched:\n{after}");

    // The handle is spent: a plan describes a tree that no longer exists.
    let err = client.call_err("apply_plan", json!({ "plan_handle": handle }));
    assert_eq!(err["reason"], "plan-handle-unknown", "second apply: {err}");
}

#[test]
fn a_handle_from_another_process_is_refused_and_nothing_is_written() {
    let proj = TempProject::new("foreign");
    proj.write("lib.php", LIB);
    proj.write("main.php", MAIN);
    let mut client = Client::start(proj.path());

    // Shaped like a real handle but minted by nobody — as from a server before a restart.
    let err = client.call_err("apply_plan", json!({ "plan_handle": "steins-plan-1-1-1" }));
    assert_eq!(err["reason"], "plan-handle-foreign-process", "error: {err}");
    let detail = err["detail"].as_str().unwrap();
    assert!(detail.contains("process"), "the error explains itself: {detail}");

    // Not a handle at all.
    let err = client.call_err("apply_plan", json!({ "plan_handle": "please-just-apply-it" }));
    assert_eq!(err["reason"], "plan-handle-malformed", "error: {err}");

    // Missing argument, unknown transform, and a path that names nothing are
    // named refusals too — never a panic, never a silent empty answer.
    let err = client.call_err("apply_plan", json!({}));
    assert_eq!(err["reason"], "missing-argument");
    let unknown = json!({ "transform": "rename-everything", "paths": [proj.path()] });
    let err = client.call_err("plan_transform", unknown);
    assert_eq!(err["reason"], "unknown-transform", "error: {err}");
    let err = client.call_err(
        "plan_transform",
        json!({ "transform": "phpdoc-to-native", "paths": [format!("{}/nope", proj.path())] }),
    );
    assert_eq!(err["reason"], "path-does-not-exist", "error: {err}");

    // Nothing above touched the tree.
    assert_eq!(proj.read("lib.php"), LIB);
    assert_eq!(proj.read("main.php"), MAIN);
}

#[test]
fn the_read_only_tools_leave_the_tree_untouched() {
    let proj = TempProject::new("readonly");
    // A dump statement: a finding that carries a fix payload (issue #114).
    let src = "<?php\n$x = 5;\n\\PHPStan\\dumpType($x);\n";
    proj.write("app.php", src);
    proj.write("lib.php", LIB);
    proj.write("main.php", MAIN);
    let mut client = Client::start(proj.path());

    let checked = client.call_ok("check", json!({ "paths": [proj.path()], "no_php": true }));
    let findings = checked["findings"].as_array().expect("findings array");
    let dump = findings.iter().find(|f| f["id"] == "debug.type").expect("the dump reports");
    assert_eq!(dump["fix"]["title"], "remove the dump statement");
    let edits = dump["fix"]["edits"].as_array().expect("fix edits");
    assert_eq!(edits.len(), 1, "one deletion: {dump}");
    assert_eq!(edits[0]["replacement"], "");
    assert_eq!(checked["profile"], "default");

    client.call_ok("list_transforms", json!({}));
    let args = json!({ "transform": "phpdoc-to-native", "paths": [proj.path()] });
    client.call_ok("plan_transform", args);

    assert_eq!(proj.read("app.php"), src, "check must not apply the fix it reports");
    assert_eq!(proj.read("lib.php"), LIB);
    assert_eq!(proj.read("main.php"), MAIN);
}

#[test]
fn a_plan_whose_targets_moved_is_refused_rather_than_spliced() {
    // The gate that makes a handle safe even inside its own process: a byte
    // span means nothing against text it was not measured in.
    let proj = TempProject::new("moved");
    proj.write("lib.php", LIB);
    proj.write("main.php", MAIN);
    let mut client = Client::start(proj.path());

    let args = json!({ "transform": "phpdoc-to-native", "paths": [proj.path()] });
    let plan = client.call_ok("plan_transform", args);
    let handle = plan["plan_handle"].as_str().expect("a plan handle").to_owned();

    // Someone edits the file between the diff and the approval.
    let moved = format!("<?php\n// a comment that shifts every offset\n{}", &LIB[6..]);
    proj.write("lib.php", &moved);

    let err = client.call_err("apply_plan", json!({ "plan_handle": handle }));
    assert_eq!(err["reason"], "tree-changed-since-plan", "error: {err}");
    assert_eq!(proj.read("lib.php"), moved, "the refusal wrote nothing");
}

#[test]
fn the_asserted_subjects_opt_in_rides_the_plan_tool_and_is_fenced_to_its_transform() {
    // ADR-0076 issue #175: opt-in follows the same code path as the CLI, so the
    // label and split count land in the same document as the diff.
    let proj = TempProject::new("asserted");
    proj.write(
        "loop.php",
        "<?php\n/** @param list<int> $xs */\nfunction scale(array $xs): array {\n    $out = [];\n    foreach ($xs as $x) {\n        $out[] = $x * 3;\n    }\n    return $out;\n}\n",
    );
    let mut client = Client::start(proj.path());

    // Fenced: on any other transform the opt-in has no defined meaning.
    let err = client.call_err(
        "plan_transform",
        json!({ "transform": "phpdoc-to-native", "paths": [proj.path()], "asserted_subjects": true }),
    );
    assert_eq!(err["reason"], "invalid-argument", "error: {err}");

    // Without the opt-in, the declared list refuses exactly as before.
    let plain = client
        .call_ok("plan_transform", json!({ "transform": "loop-to-array-map", "paths": [proj.path()] }));
    assert_eq!(plain["report"]["oracle"]["transformed"], 0, "plan: {plain}");
    assert_eq!(plain["report"]["refusals"][0]["reason"], "subject-not-proven-array");

    // With it: admitted, counted on the asserted side, and labeled per site.
    let plan = client.call_ok(
        "plan_transform",
        json!({ "transform": "loop-to-array-map", "paths": [proj.path()], "asserted_subjects": true }),
    );
    assert_eq!(plan["report"]["oracle"]["transformed"], 1, "plan: {plan}");
    assert_eq!(plan["report"]["oracle"]["transformed_asserted"], 1, "plan: {plan}");
    let admissions = plan["report"]["asserted_admissions"].as_array().expect("admissions array");
    assert_eq!(admissions.len(), 1, "plan: {plan}");
    let detail = admissions[0]["detail"].as_str().expect("admission detail");
    assert!(detail.contains("declared"), "label: {detail}");
    assert!(detail.contains("preserves keys"), "label: {detail}");
    assert!(detail.contains("post-check cannot catch"), "label: {detail}");
    assert!(plan["plan_handle"].is_string(), "an admitted site is applyable: {plan}");
    assert!(proj.read("loop.php").contains("foreach"), "the dry run must not touch the tree");
}

/// `check` answers from the generation store (issue #491): the first call
/// builds cold and publishes, the second replays the unchanged tree, and an
/// edit is reported from the source rather than from the cache.
///
/// Every reply is compared against a `--no-cache` run over the same tree, which
/// is the only property that would matter if it broke — a cache is allowed to
/// change what a call costs and nothing else (ADR-0092 §2). Warmth itself is
/// read off the store rather than off the reply: the surface says nothing about
/// its own temperature, and issue #525's ruling is why.
///
/// `--no-php` throughout, so no sidecar is involved and the run is hermetic.
#[test]
fn check_answers_from_the_generation_store() {
    let proj = TempProject::new("generation");
    proj.write("lib.php", DEFS);
    proj.write("app.php", ONE_WRONG_CALL);
    let mut client = Client::start(proj.path());
    let args = json!({ "paths": [proj.path()], "no_php": true });

    // Cold: nothing to load from, so this call is the one that publishes.
    let cold = client.call_ok("check", args.clone());
    let (published, stamp) = proj.generation().expect("the first check publishes a generation");
    assert_eq!(cold["findings"], proj.cold_findings(), "the reply is the uncached report");
    assert_eq!(cold["findings"].as_array().expect("findings array").len(), 1);

    // Warm: the same tree, so the same answer — and the store is left exactly
    // as it was, which it only can be if the run parsed no file (a rebuild
    // republishes, rewriting `CURRENT` even under an unchanged identity).
    let warm = client.call_ok("check", args.clone());
    assert_eq!(warm["findings"], cold["findings"], "a replay reports what the build reported");
    assert_eq!(warm["profile"], cold["profile"]);
    assert_eq!(warm["notices"], cold["notices"]);
    assert_eq!(
        proj.generation(),
        Some((published.clone(), stamp)),
        "an unchanged tree keeps its generation, untouched"
    );

    // An edit: the reply follows the source, and the store follows the reply.
    proj.write("app.php", TWO_WRONG_CALLS);
    let edited = client.call_ok("check", args);
    let findings = edited["findings"].as_array().expect("findings array");
    assert_eq!(findings.len(), 2, "the new call reports too: {edited}");
    assert!(findings.iter().any(|f| f["line"] == 3), "the edited line reports: {edited}");
    assert_eq!(
        edited["findings"],
        proj.cold_findings(),
        "an edited tree is answered from the source, not the cache"
    );
    assert_ne!(
        proj.generation().expect("the edit publishes").0,
        published,
        "an edit publishes a new generation"
    );

    // Nothing above wrote to the code it was asked about.
    assert_eq!(proj.read("lib.php"), DEFS);
    assert_eq!(proj.read("app.php"), TWO_WRONG_CALLS);
}

#[test]
fn unknown_tools_and_methods_are_protocol_errors() {
    let proj = TempProject::new("protocol");
    proj.write("lib.php", LIB);
    let mut client = Client::start(proj.path());

    let response = client.request("tools/call", json!({ "name": "rm_rf", "arguments": {} }));
    assert_eq!(response["error"]["code"], -32602, "unknown tool: {response}");
    let available = response["error"]["data"]["available"].as_array().expect("available list");
    assert!(available.iter().any(|t| t == "plan_transform"), "available: {response}");

    let response = client.request("resources/list", json!({}));
    assert_eq!(response["error"]["code"], -32601, "unknown method: {response}");

    // The server is still serving after refusing two calls.
    let response = client.request("ping", json!({}));
    assert!(response["result"].is_object(), "ping: {response}");
}
