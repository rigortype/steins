//! The browser-playground module (ADR-0065): `check`/`annotate` over one
//! in-memory PHP source, compiled to `wasm32-unknown-unknown` behind a
//! hand-rolled C ABI (no wasm-bindgen; JS glue is ~40 lines).
//!
//! # Posture
//!
//! No PHP is available in-browser, so [`sw_check`]/[`sw_annotate`] give the
//! **sound subset** (ADR-0004): [`NoFold`], PHP-execution findings omitted,
//! nothing false added. The posture travels as **data** (the `notice` field,
//! rendered as a banner) since wasm has no stderr. The replay entry points
//! below lift it when PHP is reachable (php-wasm, issue #64).
//!
//! # ABI
//!
//! Byte-buffer in, JSON envelope out, single-threaded by construction:
//! `sw_alloc(len)` a source buffer and write UTF-8 into it; call `sw_check`
//! or `sw_annotate` (returns `0`, leaves the envelope in a thread-local
//! buffer); read it via `sw_result_ptr()`/`sw_result_len()`; `sw_dealloc` the
//! source buffer.
//!
//! # Replay
//!
//! `Folder::fold` is synchronous and php-wasm's JS API is not, so the fold
//! surface is reached by a **request-replay fixpoint** (ADR-0066):
//! `sw_check_replay`/`sw_annotate_replay` take an extra JSON-object buffer
//! (request key → raw JSON-RPC `result`) and add `"pending"` (unanswered
//! requests, first-occurrence order, deduped) and `"boot"` (engine surface
//! per the shared fold policy) to the envelope.
//!
//! Caller's loop: call with the table it has (`{}` first pass); empty
//! `pending` means render; otherwise answer each pending key (parses as
//! `{"method", "params"}`; the answer is `steins_handle`'s raw `result` for
//! that call), insert under the SAME key strings, repeat.
//!
//! **Non-empty `pending` ⇒ NoFold-degraded results that MUST NOT be
//! rendered** (would flicker as the loop converges). Termination is the
//! answered set strictly growing; exhausting the caller's iteration cap falls
//! back to the non-replay entry points, never a half-converged render.
//!
//! `boot` makes the precision boundary legible (issue #61 S2) — which lanes
//! are live is known only to the booted engine's policy — and travels as
//! data for the same reason `notice` does.
//!
//! Every envelope carries `"ok"`: `true` + payload, or `false` + `"error"`
//! (unknown profile, or invalid UTF-8). Never traps: a PHP-rejected snippet
//! parses with recovery, reported in `parse_errors` — since ADR-0079
//! (issue #180) also as a `syntax.unparsable` finding, with `parse_errors`
//! the per-error detail (position, not just first + count).
//!
//! `findings` mirrors the CLI's `--format json` schema key for key. The
//! pipeline is the CLI's minus channels meaningless for a pasted snippet:
//! vendor filtering, `[[policy]]`, baseline. Inline `@steins-ignore` **is**
//! applied, including `suppress.unmatched` anti-rot.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use steins_db::{PluginFacts, Project, ProjectLayout, SourceFile, SteinsDatabase, parse};
use steins_infer::profile::ProfileConfigs;
use steins_infer::suppress::apply_inline_ignores;
use steins_infer::{
    Folder, NoFold, SOUND_SUBSET_NOTICE, TableFolder, annotate_project, check_project_with_runtime,
};

/// The diagnostic path a playground snippet analyzes under (stable,
/// self-describing in messages).
const SNIPPET_PATH: &str = "playground.php";

thread_local! {
    /// The last envelope from [`sw_check`]/[`sw_annotate`]; one wasm instance is
    /// single-threaded, so one buffer is the whole story.
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn set_result(json: serde_json::Value) -> i32 {
    let bytes = json.to_string().into_bytes();
    RESULT.with(|r| *r.borrow_mut() = bytes);
    0
}

fn error_envelope(message: &str) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": message })
}

/// Allocate `len` bytes of wasm memory for the caller to write into. Returns a
/// pointer the caller passes back to [`sw_check`]/[`sw_annotate`] and finally to
/// [`sw_dealloc`]. A zero `len` returns a dangling-but-valid pointer.
#[unsafe(no_mangle)]
pub extern "C" fn sw_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Release a buffer obtained from [`sw_alloc`].
///
/// # Safety
///
/// `ptr` must come from [`sw_alloc`] with the same `len` and not be used after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        // SAFETY: contract above — this reconstitutes the sw_alloc allocation.
        unsafe { drop(Vec::from_raw_parts(ptr, 0, len.max(1))) };
    }
}

/// The pointer half of the result-buffer accessor pair.
#[unsafe(no_mangle)]
pub extern "C" fn sw_result_ptr() -> *const u8 {
    RESULT.with(|r| r.borrow().as_ptr())
}

/// The length half of the result-buffer accessor pair.
#[unsafe(no_mangle)]
pub extern "C" fn sw_result_len() -> usize {
    RESULT.with(|r| r.borrow().len())
}

/// # Safety
/// `ptr`/`len` must describe readable wasm memory (normally an [`sw_alloc`]
/// buffer the caller filled).
unsafe fn read_str<'a>(ptr: *const u8, len: usize) -> Result<&'a str, ()> {
    if ptr.is_null() && len > 0 {
        return Err(());
    }
    let bytes: &[u8] =
        if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len) } };
    std::str::from_utf8(bytes).map_err(|_| ())
}

/// Check `src` under the built-in profile named by `prof` (empty = `default`).
/// Always returns `0`; the envelope's `ok` field is the real verdict.
///
/// # Safety
/// Both pointer/length pairs must describe readable wasm memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_check(
    src_ptr: *const u8,
    src_len: usize,
    prof_ptr: *const u8,
    prof_len: usize,
) -> i32 {
    let Ok(source) = (unsafe { read_str(src_ptr, src_len) }) else {
        return set_result(error_envelope("source is not valid UTF-8"));
    };
    let Ok(prof) = (unsafe { read_str(prof_ptr, prof_len) }) else {
        return set_result(error_envelope("profile name is not valid UTF-8"));
    };
    let selected = if prof.is_empty() { None } else { Some(prof) };
    set_result(check_impl(source, selected))
}

/// Annotate `src` (the `steins annotate` margin facts). Always returns `0`.
///
/// # Safety
/// The pointer/length pair must describe readable wasm memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_annotate(src_ptr: *const u8, src_len: usize) -> i32 {
    let Ok(source) = (unsafe { read_str(src_ptr, src_len) }) else {
        return set_result(error_envelope("source is not valid UTF-8"));
    };
    set_result(annotate_impl(source))
}

/// [`sw_check`] with a **replay table** (ADR-0066): `table` maps request key to
/// the raw JSON-RPC `result` (`{}` starts a loop). Envelope gains `"pending"`
/// (always present, empty iff complete) — see the module docs for the
/// non-empty-`pending` rule and the caller loop.
///
/// # Safety
/// All three pointer/length pairs must describe readable wasm memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_check_replay(
    src_ptr: *const u8,
    src_len: usize,
    prof_ptr: *const u8,
    prof_len: usize,
    table_ptr: *const u8,
    table_len: usize,
) -> i32 {
    let Ok(source) = (unsafe { read_str(src_ptr, src_len) }) else {
        return set_result(error_envelope("source is not valid UTF-8"));
    };
    let Ok(prof) = (unsafe { read_str(prof_ptr, prof_len) }) else {
        return set_result(error_envelope("profile name is not valid UTF-8"));
    };
    let table = match unsafe { read_table(table_ptr, table_len) } {
        Ok(t) => t,
        Err(e) => return set_result(error_envelope(e)),
    };
    let selected = if prof.is_empty() { None } else { Some(prof) };

    // A FRESH folder per call, by construction: the ABI takes the table by value
    // and drops the folder here, so a stale decline can never outlive the answer
    // that fixes it.
    let mut folder = TableFolder::with_table(table);
    let mut envelope = check_with_folder(source, selected, &mut folder);
    set_result(with_replay_extras(&mut envelope, &mut folder))
}

/// [`sw_annotate`] with a **replay table** — the annotate twin of
/// [`sw_check_replay`], same `pending` contract.
///
/// # Safety
/// Both pointer/length pairs must describe readable wasm memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_annotate_replay(
    src_ptr: *const u8,
    src_len: usize,
    table_ptr: *const u8,
    table_len: usize,
) -> i32 {
    let Ok(source) = (unsafe { read_str(src_ptr, src_len) }) else {
        return set_result(error_envelope("source is not valid UTF-8"));
    };
    let table = match unsafe { read_table(table_ptr, table_len) } {
        Ok(t) => t,
        Err(e) => return set_result(error_envelope(e)),
    };
    let mut folder = TableFolder::with_table(table);
    let mut envelope = annotate_with_folder(source, &mut folder);
    set_result(with_replay_extras(&mut envelope, &mut folder))
}

/// Read the replay table buffer: a JSON **object** of key → raw `result`
/// value. Anything else is the `error_envelope` path — a malformed table is a
/// caller bug, not a fold outcome.
///
/// # Safety
/// The pointer/length pair must describe readable wasm memory.
unsafe fn read_table(
    ptr: *const u8,
    len: usize,
) -> Result<HashMap<String, serde_json::Value>, &'static str> {
    let text = unsafe { read_str(ptr, len) }.map_err(|()| "replay table is not valid UTF-8")?;
    if text.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "replay table is not valid JSON")?;
    match value {
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        _ => Err("replay table must be a JSON object"),
    }
}

/// Attach the replay-only `"boot"`/`"pending"` keys to `envelope`. Both are
/// always present on a replay envelope (never on plain [`sw_check`]/
/// [`sw_annotate`] envelopes, which stay byte-identical to ADR-0065's).
///
/// Order matters: the summary is taken **before** the pending list, so a run
/// that cannot describe its own boot surface reports that as pending and
/// converges one iteration later with `boot` filled in — a converged run
/// therefore always carries a complete `boot`.
fn with_replay_extras(
    envelope: &mut serde_json::Value,
    folder: &mut TableFolder,
) -> serde_json::Value {
    let boot = boot_json(&folder.surface_summary());
    let pending = folder.take_pending();
    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("boot".to_owned(), boot);
        obj.insert("pending".to_owned(), serde_json::json!(pending));
    }
    envelope.take()
}

/// The boot object (issue #64 S3): the engine surface **as the shared policy
/// sees it**, as data — fields come from [`steins_infer::SurfaceSummary`],
/// which reads the same helpers that gate admission.
///
/// Since ADR-0028's 2026-08-14 amendment, refused and unverified fold names
/// travel as TWO fields: refused rows have a recorded divergence, unverified
/// rows have nothing measured, and §4 of that amendment forbids merging them
/// into one list.
fn boot_json(s: &steins_infer::SurfaceSummary) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "label": s.label,
        "php_version": s.php_version,
        "int_size": s.int_size,
        "fold_lane": s.fold_lane.as_str(),
        "fold_total": s.fold_total,
        "fold_portable": s.fold_portable,
        "curated_rows": s.curated_rows,
        "absence_family": s.absence_family,
    });
    // Named only where it's the boundary: portable subset refusals are exactly
    // what the visitor doesn't get; elsewhere the lane already says it all.
    if s.fold_lane == steins_infer::FoldLane::PortableSubset {
        obj["refused_folds"] = serde_json::json!(s.refused_folds);
        // …and, beside the names, WHY each is refused. The panel used to say
        // every refused name renders an integer in the machine's own word,
        // which stopped being true the moment `preg_split` was refused for a
        // PCRE build option — a sentence that can go false as the table grows
        // is a sentence the table should be supplying instead.
        obj["refusals"] = serde_json::json!(
            s.refusals
                .iter()
                .map(|r| serde_json::json!({ "name": r.name, "axis": r.axis.as_str(), "witness": r.witness }))
                .collect::<Vec<_>>()
        );
        obj["unverified_folds"] = serde_json::json!(s.unverified_folds);
    }
    obj
}

/// The target-agnostic body of [`sw_check`] — also what the native tests pin.
fn check_impl(source: &str, selected: Option<&str>) -> serde_json::Value {
    check_with_folder(source, selected, &mut NoFold)
}

/// [`check_impl`] over an arbitrary folder — the ONE analysis body; sound-subset
/// and replay entry points differ only in the folder they hand it.
fn check_with_folder(
    source: &str,
    selected: Option<&str>,
    folder: &mut dyn Folder,
) -> serde_json::Value {
    // No steins.toml in a browser: `selected` resolves against built-ins alone;
    // an unknown name is the CLI's exit-2 config error, delivered as data.
    let configs = ProfileConfigs(BTreeMap::new());
    let surface = match configs.resolve(selected) {
        Ok(s) => s,
        Err(e) => return error_envelope(&e.to_string()),
    };

    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, SNIPPET_PATH.to_owned(), source.to_owned());
    let project = Project::new(&db, vec![file], ProjectLayout::fallback(), PluginFacts::none());
    // `warning_handler_abort = true` is the CLI DEFAULT (ADR-0049 §7): only
    // `[runtime] warning-handler = "null"` opts out, and a browser snippet has
    // no steins.toml to set it.
    let mut findings = check_project_with_runtime(&db, project, folder, true);

    // The CLI pipeline (ADR-0050 §6) minus the snippet-meaningless channels:
    // vendor (nothing here is vendored), policy (no config), baseline (no fs).
    findings.retain(|d| surface.is_surfaced(d));
    let tree = parse(&db, file);
    let pairs: Vec<(String, &steins_syntax::SourceTree)> =
        vec![(SNIPPET_PATH.to_owned(), tree)];
    let inline = apply_inline_ignores(findings, &pairs);

    let mut displayed = inline.kept;
    displayed.extend(inline.meta);
    displayed.sort_by(|a, b| (a.line, a.column, a.id).cmp(&(b.line, b.column, b.id)));

    let findings_json: Vec<serde_json::Value> = displayed
        .iter()
        .map(|d| {
            let mut obj = serde_json::json!({
                "id": d.id,
                "layer": steins_infer::layer(d.id).map(steins_infer::Layer::as_str),
                "level": surface.level(d.id).as_str(),
                "path": d.path,
                "line": d.line,
                "column": d.column,
                "message": d.message,
            });
            if let Some(facet) = d.facet {
                obj[facet.key()] = serde_json::Value::String(facet.value().to_owned());
            }
            obj
        })
        .collect();

    let parse_errors: Vec<serde_json::Value> = tree
        .parse_errors()
        .iter()
        .map(|e| {
            serde_json::json!({
                "line": tree.position(e.span.start).line,
                "message": e.message,
            })
        })
        .collect();

    serde_json::json!({
        "ok": true,
        "notice": SOUND_SUBSET_NOTICE,
        "profile": surface.name,
        "findings": findings_json,
        "suppressed": inline.suppressed,
        "parse_errors": parse_errors,
    })
}

/// The target-agnostic body of [`sw_annotate`].
fn annotate_impl(source: &str) -> serde_json::Value {
    annotate_with_folder(source, &mut NoFold)
}

/// [`annotate_impl`] over an arbitrary folder — the annotate twin of
/// [`check_with_folder`].
fn annotate_with_folder(source: &str, folder: &mut dyn Folder) -> serde_json::Value {
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, SNIPPET_PATH.to_owned(), source.to_owned());
    let project = Project::new(&db, vec![file], ProjectLayout::fallback(), PluginFacts::none());
    let facts = annotate_project(&db, project, file, folder);

    let lines: Vec<serde_json::Value> = facts
        .iter()
        .map(|f| serde_json::json!({ "line": f.line, "text": f.body() }))
        .collect();

    serde_json::json!({
        "ok": true,
        "notice": SOUND_SUBSET_NOTICE,
        "lines": lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The envelope contract the JS glue reads: shape, notice-as-data, and a
    /// real finding with a stable id on the seeded snippet.
    #[test]
    fn check_envelope_carries_notice_and_findings() {
        let src = "<?php\nfunction f(int $x): int { return $x; }\nf(\"abc\");\n";
        let v = check_impl(src, None);
        assert_eq!(v["ok"], true);
        assert_eq!(v["notice"], SOUND_SUBSET_NOTICE);
        assert_eq!(v["profile"], "default");
        let ids: Vec<&str> =
            v["findings"].as_array().unwrap().iter().map(|f| f["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"type.argument-mismatch"), "got {ids:?}");
        assert_eq!(v["parse_errors"].as_array().unwrap().len(), 0);
    }

    /// An unknown profile is the exit-2 config error as data, never a trap.
    #[test]
    fn unknown_profile_is_structured_error() {
        let v = check_impl("<?php\n", Some("nope"));
        assert_eq!(v["ok"], false);
        let msg = v["error"].as_str().unwrap();
        assert!(msg.contains("unknown profile"), "got {msg}");
        assert!(msg.contains("strict"), "the ladder names its rungs: {msg}");
    }

    /// The four built-in profiles all resolve, strict included, same as the CLI.
    #[test]
    fn builtin_profiles_resolve() {
        for p in ["default", "contracts", "throws-direct", "strict"] {
            let v = check_impl("<?php\n", Some(p));
            assert_eq!(v["ok"], true, "{p}");
            assert_eq!(v["profile"], p);
        }
    }

    /// A syntactically broken snippet analyzes with recovery and REPORTS its
    /// parse errors — the playground states what the CLI today keeps silent.
    #[test]
    fn broken_syntax_reports_parse_errors() {
        let v = check_impl("<?php\nfunction f( {\n", None);
        assert_eq!(v["ok"], true);
        assert!(!v["parse_errors"].as_array().unwrap().is_empty());
    }

    /// Inline `@steins-ignore` works in the playground exactly as in the CLI,
    /// including the anti-rot meta finding when it matches nothing.
    #[test]
    fn inline_ignore_and_unmatched_meta() {
        let suppressed = "<?php\nfunction f(int $x): int { return $x; }\n// @steins-ignore type.argument-mismatch\nf(\"abc\");\n";
        let v = check_impl(suppressed, None);
        assert_eq!(v["suppressed"], 1);
        let unmatched = "<?php\n// @steins-ignore call.on-null\n$x = 1;\n";
        let v = check_impl(unmatched, None);
        let ids: Vec<&str> =
            v["findings"].as_array().unwrap().iter().map(|f| f["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"suppress.unmatched"), "got {ids:?}");
    }

    /// The annotate envelope: per-line margin facts through the same module.
    #[test]
    fn annotate_envelope_carries_line_facts() {
        let src = "<?php\nfunction f(): int { return 1; }\n";
        let v = annotate_impl(src);
        assert_eq!(v["ok"], true);
        assert_eq!(v["notice"], SOUND_SUBSET_NOTICE);
        assert!(!v["lines"].as_array().unwrap().is_empty());
    }

    /// The trace annotation (ADR-0074): the scanner + `stmt_docblock` adoption
    /// are zero-dep, so `/** @psalm-trace $x */` reports `debug.trace` exactly
    /// as the CLI does — warn, debug lane, at the tag's own position.
    #[test]
    fn trace_annotation_emits_in_the_playground() {
        let src = "<?php\n/** @psalm-trace $x */\n$x = 'GET';\n";
        let v = check_impl(src, None);
        assert_eq!(v["ok"], true);
        let traces: Vec<&serde_json::Value> = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["id"] == "debug.trace")
            .collect();
        assert_eq!(traces.len(), 1, "got {:?}", v["findings"]);
        let t = traces[0];
        assert_eq!(t["message"], "traced type of $x: 'GET'");
        assert_eq!(t["layer"], "debug");
        assert_eq!(t["level"], "warn");
        assert_eq!(t["line"], 2, "reported at the tag's line, not the statement's");
    }
}

/// The replay ABI (ADR-0066), pinned natively: the `pending` contract, the
/// malformed-table path, and a fully-answered canned table folding the flagship.
///
/// The table is captured from the differential oracle in
/// `steins-infer/tests/replay_fold.rs` (a real `php` answered these requests)
/// and hardcoded so the pin survives without a PHP dependency — only the
/// extension list is trimmed. Key strings are hardcoded deliberately: they are
/// the interchange format S2's loop echoes back, so a silent shape change must
/// break a test.
#[cfg(test)]
mod replay {
    use super::*;

    /// The issue-#59/#60 flagship: a project call in argument position whose body
    /// folds through the engine.
    const FLAGSHIP: &str = "<?php\n\
        function greet(int $times, string $name): string {\n\
            return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
        }\n\
        \\PHPStan\\dumpType(greet(2, \"World\"));\n";

    const ENV_KEY: &str = r#"{"method":"env","params":{}}"#;
    const FOLD_KEY: &str =
        r#"{"method":"fold","params":{"function":"str_repeat","args":["Hello, World! ",2],"strict":false}}"#;
    const REFLECT_KEY: &str = r#"{"method":"reflect","params":{"target":"greet"}}"#;

    fn answered_table() -> HashMap<String, serde_json::Value> {
        HashMap::from([
            (
                ENV_KEY.to_owned(),
                serde_json::json!({
                    "php_version": "8.5.8",
                    "extensions": ["Core", "standard"],
                    "sapi": "cli",
                    "int_size": 8,
                }),
            ),
            (
                FOLD_KEY.to_owned(),
                serde_json::json!({
                    "kind": "value",
                    "value": "Hello, World! Hello, World! ",
                    "type": "string",
                }),
            ),
            (
                REFLECT_KEY.to_owned(),
                serde_json::json!({
                    "kind": "reflection",
                    "target": "greet",
                    "exists": false,
                    "function": false,
                    "class_like": false,
                    "return_type": serde_json::Value::Null,
                    "return_type_tentative": false,
                }),
            ),
        ])
    }

    /// The `env` answer php-wasm 0.1.0 actually gives: PHP 8.5.2 (pinned minor)
    /// on a 32-bit `embed` build — this row, not the version string alone,
    /// decides what is live.
    fn php_wasm_env() -> serde_json::Value {
        serde_json::json!({
            "php_version": "8.5.2",
            "extensions": ["Core", "standard"],
            "sapi": "embed",
            "int_size": 4,
        })
    }

    fn check_replay(
        source: &str,
        table: HashMap<String, serde_json::Value>,
    ) -> serde_json::Value {
        let mut folder = TableFolder::with_table(table);
        let mut envelope = check_with_folder(source, None, &mut folder);
        with_replay_extras(&mut envelope, &mut folder)
    }

    fn pending_of(v: &serde_json::Value) -> Vec<String> {
        v["pending"]
            .as_array()
            .expect("pending is always present")
            .iter()
            .map(|k| k.as_str().expect("a pending key is a string").to_owned())
            .collect()
    }

    /// An empty table: a real envelope plus the questions the run wants answered.
    /// The first is always `env` — the integer-width gate (issue #64) won't
    /// dispatch a value question to an engine whose arithmetic isn't established.
    #[test]
    fn an_empty_table_returns_an_ok_envelope_and_pending_requests() {
        let v = check_replay(FLAGSHIP, HashMap::new());
        assert_eq!(v["ok"], true);
        let pending = pending_of(&v);
        assert!(!pending.is_empty(), "an unanswered run reports its questions");
        assert!(pending.contains(&ENV_KEY.to_owned()), "got {pending:?}");
        // Every key parses as the request it stands for.
        for key in &pending {
            let req: serde_json::Value = serde_json::from_str(key).expect("a key is JSON");
            assert!(req.get("method").and_then(serde_json::Value::as_str).is_some(), "{key}");
            assert!(req.get("params").is_some(), "{key}");
        }
    }

    /// A fully-answered table: the flagship folds, and `pending` is empty — the
    /// one state in which a caller may render the result.
    #[test]
    fn a_fully_answered_table_folds_the_flagship_with_nothing_pending() {
        let v = check_replay(FLAGSHIP, answered_table());
        assert_eq!(v["ok"], true);
        assert_eq!(pending_of(&v), Vec::<String>::new(), "the fixpoint is reached");
        let dumps: Vec<&str> = v["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .filter(|f| f["id"] == "debug.type")
            .map(|f| f["message"].as_str().expect("message"))
            .collect();
        assert_eq!(dumps, vec!["dumped type: 'Hello, World! Hello, World! '"]);
    }

    /// The issue-#64 acceptance criterion: php-wasm 0.1.0 is PHP **8.5.2** built
    /// **32-bit**, and the flagship still folds — `str_repeat` is on the verified
    /// portable subset (ADR-0066 S1.5 amendment) and every integer in the call
    /// is in range.
    #[test]
    fn the_flagship_folds_on_a_32_bit_engine() {
        let mut table = answered_table();
        table.insert(ENV_KEY.to_owned(), php_wasm_env());
        let v = check_replay(FLAGSHIP, table);
        assert_eq!(v["ok"], true);
        assert_eq!(pending_of(&v), Vec::<String>::new(), "the fixpoint is reached");
        let dumps: Vec<&str> = v["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .filter(|f| f["id"] == "debug.type")
            .map(|f| f["message"].as_str().expect("message"))
            .collect();
        assert_eq!(dumps, vec!["dumped type: 'Hello, World! Hello, World! '"]);
    }

    /// …and a width-REFUSED builtin stays declined on the same 32-bit engine
    /// even with the answer in the table. `intval("3000000000")` silently
    /// saturates to `2147483647` on 32-bit vs `3000000000` on 64-bit — the gate
    /// is upstream of the table, so a pre-answered wrong literal cannot surface.
    #[test]
    fn a_width_refused_builtin_stays_declined_on_a_32_bit_engine() {
        const SRC: &str = "<?php\n$x = intval(\"3000000000\");\n\\PHPStan\\dumpType($x);\n";
        const INTVAL_KEY: &str =
            r#"{"method":"fold","params":{"function":"intval","args":["3000000000"],"strict":false}}"#;
        const INTVAL_REFLECT_KEY: &str = r#"{"method":"reflect","params":{"target":"intval"}}"#;
        let mut table = HashMap::from([
            (ENV_KEY.to_owned(), php_wasm_env()),
            (
                INTVAL_KEY.to_owned(),
                serde_json::json!({ "kind": "value", "value": 2_147_483_647_i64, "type": "int" }),
            ),
            // The declined fold falls back to the reflected return envelope
            // (ADR-0056 R1, width-independent) — the run still reaches fixpoint,
            // one rung less precise.
            (
                INTVAL_REFLECT_KEY.to_owned(),
                serde_json::json!({
                    "kind": "reflection",
                    "target": "intval",
                    "exists": true,
                    "function": true,
                    "class_like": false,
                    "return_type": "int",
                    "return_type_tentative": false,
                }),
            ),
        ]);
        let v = check_replay(SRC, table.clone());
        assert_eq!(v["ok"], true);
        assert_eq!(pending_of(&v), Vec::<String>::new(), "a refused fold asks nothing");
        let dumps: Vec<&str> = v["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .filter(|f| f["id"] == "debug.type")
            .map(|f| f["message"].as_str().expect("message"))
            .collect();
        assert_eq!(dumps, vec!["dumped type: int"], "the saturated literal never lands");
        // The SAME table on a 64-bit engine folds it — so the decline is the
        // width and not a missing answer.
        table.insert(
            ENV_KEY.to_owned(),
            serde_json::json!({
                "php_version": "8.5.8",
                "extensions": ["Core", "standard"],
                "sapi": "cli",
                "int_size": 8,
            }),
        );
        let v = check_replay(SRC, table);
        let dumps: Vec<&str> = v["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .filter(|f| f["id"] == "debug.type")
            .map(|f| f["message"].as_str().expect("message"))
            .collect();
        assert_eq!(dumps, vec!["dumped type: 2147483647"]);
    }

    /// The boot object on the machine the browser actually has (issue #64 S3):
    /// the boundary a visitor must be able to read, as data.
    ///
    /// Each field is pinned against the gate it reports: portable lane,
    /// curated rows DECLINED (ADR-0066's amendment keeps Gate 2's `int_size == 8`
    /// leg), absence family LIVE (existence is not arithmetic), refused names
    /// taken from the catalog so a new refusal appears without editing JS.
    ///
    /// `fold_total` moved 46 → 48 with ADR-0028's 2026-08-14 wave 1
    /// (`array_merge`, `explode`) — the count growing, not the boundary moving:
    /// `fold_portable` unchanged, and the two new names land in `unverified_folds`
    /// (no divergence to report), not `refused_folds` — see `boot_json` for why
    /// those are separate fields.
    ///
    /// Issue #354 moved 48 → 53 and, unlike wave 1, moved the boundary in both
    /// directions at once: `fold_portable` 37 → 40 (`str_split`, `array_fill`,
    /// `array_unique` probed clean, so the browser folds them), and
    /// `refused_folds` gained `range` and `preg_split`, which the page now
    /// names. `unverified_folds` is untouched — a probed name never passes
    /// through that class.
    ///
    /// The alias slice then moved 53 → 57 in the safe direction only: `join`,
    /// `chop`, `sizeof` and `doubleval` are PHP's own second spellings for
    /// names already here, so the browser folds four more and refuses nothing
    /// new.
    ///
    /// Wave 2 moved 57 → 63 in the safe direction only: `strpos`/`stripos`/
    /// `strrpos` and `round`/`floor`/`ceil`, six names whose only integer
    /// parameter declines rather than diverges on the narrow engine. The two it
    /// withdrew came back in issue #382 once the seam could gate them, and they
    /// moved the counts in OPPOSITE directions: `array_filter` folds here
    /// (portable 50 → 51), `preg_match` does not (refused 11 → 12, beside
    /// `preg_split` and for the same PCRE build option).
    #[test]
    fn the_boot_object_describes_a_32_bit_engine() {
        let mut table = answered_table();
        table.insert(ENV_KEY.to_owned(), php_wasm_env());
        let v = check_replay(FLAGSHIP, table);
        assert_eq!(pending_of(&v), Vec::<String>::new(), "a converged run describes its engine");
        let boot = &v["boot"];
        assert_eq!(boot["php_version"], "8.5.2");
        assert_eq!(boot["int_size"], 4);
        assert_eq!(boot["label"], "PHP 8.5.2 (2 extensions)");
        assert_eq!(boot["fold_lane"], "portable_subset");
        assert_eq!(boot["curated_rows"], false, "a curated row is pinned to a machine too");
        assert_eq!(boot["absence_family"], true, "existence is not arithmetic");
        assert_eq!(boot["fold_total"], 65);
        assert_eq!(
            boot["fold_portable"], 53,
            "…and issue #382 measured the last two unverified rows into it, so the ALLOWLIST \
             stands still at 65 while this engine's share moves 51 -> 53"
        );
        assert_eq!(
            boot["refused_folds"],
            serde_json::json!(steins_catalog::refused_names()),
            "the refusals are the catalog's refused rows"
        );
        assert_eq!(
            boot["refused_folds"],
            serde_json::json!([
                "abs",
                "intval",
                "sprintf",
                "dechex",
                "decbin",
                "decoct",
                "bindec",
                "hexdec",
                "version_compare",
                "range",
                "preg_split",
                "preg_match"
            ])
        );
        // Beside the names, the reason each row is refused — the field the
        // boundary panel groups by, so its sentences cannot go false as the
        // table grows the way the hand-written one did.
        let refusals = boot["refusals"].as_array().expect("refusals is an array");
        assert_eq!(refusals.len(), 12, "one entry per refused row");
        assert_eq!(refusals[0]["name"], "abs");
        assert_eq!(refusals[0]["axis"], "integer_width");
        assert!(
            refusals[0]["witness"].as_str().expect("witness").contains(" / "),
            "a witness shows both engines' answers"
        );
        for name in ["preg_split", "preg_match"] {
            let preg = refusals.iter().find(|r| r["name"] == name).expect("a PCRE row");
            assert_eq!(preg["axis"], "build_option", "{name} is not about the word size");
        }
        assert_eq!(
            refusals.iter().filter(|r| r["axis"] == "build_option").count(),
            2,
            "and those two are the only ones — same matcher, same build option"
        );
        assert_eq!(
            refusals.iter().map(|r| r["name"].clone()).collect::<Vec<_>>(),
            boot["refused_folds"].as_array().expect("refused_folds").clone(),
            "every refused row has a reason, in the same order"
        );
        assert_eq!(
            boot["unverified_folds"],
            serde_json::json!(steins_catalog::unverified_names()),
            "the unverified rows are the catalog's own, in their own field"
        );
        // The class is empty now (issue #382 measured both rows into `portable`),
        // and the field still travels: an empty list is "nothing is unmeasured",
        // which is a different claim from a missing field ("this lane has no
        // opinion") and the panel says so.
        assert_eq!(boot["unverified_folds"], serde_json::json!([] as [&str; 0]));
    }

    /// …and on a 64-bit engine at the pinned minor the whole surface is live, so
    /// the refusals are not named at all: there are none to name.
    #[test]
    fn the_boot_object_describes_a_64_bit_engine() {
        let v = check_replay(FLAGSHIP, answered_table());
        assert_eq!(pending_of(&v), Vec::<String>::new());
        let boot = &v["boot"];
        assert_eq!(boot["php_version"], "8.5.8");
        assert_eq!(boot["int_size"], 8);
        assert_eq!(boot["fold_lane"], "full");
        assert_eq!(boot["curated_rows"], true);
        assert_eq!(boot["absence_family"], true);
        assert!(boot.get("refused_folds").is_none(), "nothing is refused on the full lane");
        assert!(boot.get("unverified_folds").is_none(), "…and nothing is unverified there either");
    }

    /// A run that could not reach the engine describes nothing, and SAYS so:
    /// `env` is pending, so the next iteration carries the description. A
    /// converged run always has a complete boot object — no null check per field.
    #[test]
    fn an_unanswered_run_has_an_empty_boot_and_asks_for_it() {
        let v = check_replay("<?php\n$a = 1;\n", HashMap::new());
        let boot = &v["boot"];
        assert!(boot["label"].is_null());
        assert!(boot["php_version"].is_null());
        assert!(boot["int_size"].is_null());
        assert_eq!(boot["fold_lane"], "declined", "an unknown width folds nothing");
        assert_eq!(boot["curated_rows"], false);
        assert_eq!(boot["absence_family"], false);
        assert!(boot.get("refused_folds").is_none());
        assert!(boot.get("unverified_folds").is_none());
        assert_eq!(
            pending_of(&v),
            vec![ENV_KEY.to_owned()],
            "a snippet with no engine question still asks for the boot surface"
        );
    }

    /// The annotate lane carries the same boot object — the engine bar reads one
    /// envelope, and the two lanes must not disagree about the machine.
    #[test]
    fn the_annotate_envelope_carries_the_same_boot_object() {
        let mut table = answered_table();
        table.insert(ENV_KEY.to_owned(), php_wasm_env());
        let mut folder = TableFolder::with_table(table.clone());
        let mut envelope = annotate_with_folder(FLAGSHIP, &mut folder);
        let annotated = with_replay_extras(&mut envelope, &mut folder);
        let checked = check_replay(FLAGSHIP, table);
        assert_eq!(annotated["boot"], checked["boot"]);
    }

    /// The same source through the sound-subset entry point stays NoFold — the
    /// replay exports are additive, `sw_check` byte-identical to before. Neither
    /// replay-only key may appear: an extra envelope key IS a behaviour change.
    #[test]
    fn the_non_replay_entry_point_is_unchanged() {
        let plain = check_impl(FLAGSHIP, None);
        assert!(plain.get("pending").is_none(), "no pending key on the sound-subset envelope");
        assert!(plain.get("boot").is_none(), "no boot key on the sound-subset envelope");
        let annotated = annotate_impl(FLAGSHIP);
        assert!(annotated.get("pending").is_none(), "nor on the annotate twin");
        assert!(annotated.get("boot").is_none(), "nor on the annotate twin");
        let dumps: Vec<&str> = plain["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .filter(|f| f["id"] == "debug.type")
            .map(|f| f["message"].as_str().expect("message"))
            .collect();
        assert_eq!(dumps, vec!["dumped type: string"], "engine-not-loaded behavior unchanged");
    }

    /// Annotate rides the same loop and carries the same `pending` contract.
    #[test]
    fn annotate_replay_reaches_its_fixpoint_too() {
        let mut folder = TableFolder::with_table(HashMap::new());
        let mut envelope = annotate_with_folder(FLAGSHIP, &mut folder);
        let first = with_replay_extras(&mut envelope, &mut folder);
        assert_eq!(first["ok"], true);
        assert!(!pending_of(&first).is_empty());

        let mut folder = TableFolder::with_table(answered_table());
        let mut envelope = annotate_with_folder(FLAGSHIP, &mut folder);
        let done = with_replay_extras(&mut envelope, &mut folder);
        assert_eq!(pending_of(&done), Vec::<String>::new());
        assert!(!done["lines"].as_array().expect("lines").is_empty());
    }

    /// A malformed table is the caller's bug, delivered as data on the existing
    /// error path — never a trap, and never a silently empty table.
    #[test]
    fn a_malformed_table_is_a_structured_error() {
        for (bytes, want) in [
            ("not json", "replay table is not valid JSON"),
            ("[1, 2]", "replay table must be a JSON object"),
            ("\"a string\"", "replay table must be a JSON object"),
        ] {
            let table = unsafe { read_table(bytes.as_ptr(), bytes.len()) };
            assert_eq!(table.err(), Some(want), "input {bytes:?}");
        }
        // An empty buffer is the same as `{}` — the natural first call.
        let empty = unsafe { read_table(std::ptr::null(), 0) }.expect("empty buffer is a table");
        assert!(empty.is_empty());
    }

    /// An unknown profile still errors the way it does without a table, and the
    /// envelope still carries `pending` — the key is unconditional.
    #[test]
    fn an_unknown_profile_still_errors_under_replay() {
        let mut folder = TableFolder::with_table(answered_table());
        let mut envelope = check_with_folder(FLAGSHIP, Some("nope"), &mut folder);
        let v = with_replay_extras(&mut envelope, &mut folder);
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().expect("error").contains("unknown profile"));
        assert!(v.get("pending").is_some(), "pending is always present");
    }
}

#[cfg(test)]
mod strict_leg {
    use super::*;

    /// The strict rung fires exactly as the CLI does, pinning the
    /// `warning_handler_abort = true` default (see `check_with_folder`): the
    /// playground must never be quieter than `steins check --profile strict`.
    #[test]
    fn strict_fixture_fires_maybe_missing() {
        let src = "<?php\n/** @param array{a?: string} $d */\nfunction f(array $d): void { $x = $d[\"a\"]; }\n";
        let v = check_impl(src, Some("strict"));
        let ids: Vec<&str> = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"offset.maybe-missing"), "got {ids:?}");
        // And the same read is quiet one rung down — the ladder is the point.
        let v = check_impl(src, Some("contracts"));
        assert_eq!(v["findings"].as_array().unwrap().len(), 0, "quiet at contracts");
    }
}

