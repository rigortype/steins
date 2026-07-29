//! The browser-playground module (ADR-0065): `check` and `annotate` over a single
//! in-memory PHP source, compiled to `wasm32-unknown-unknown` and exposed through
//! a hand-rolled C ABI. No wasm-bindgen: the JS glue is ~40 lines the playground
//! owns, and the wasm dependency graph stays exactly the analysis graph.
//!
//! # Posture
//!
//! The browser has no PHP, so every run here is the **sound subset** (ADR-0004):
//! the folder is [`NoFold`], findings that require executing PHP are omitted, and
//! nothing false is added. On the CLI that posture is announced on stderr; a wasm
//! module has no stderr a user reads, so the notice travels as **data** — the
//! `notice` field of every envelope — and the frontend renders it as a banner.
//!
//! # ABI
//!
//! Byte-buffer in, JSON envelope out, single-threaded by construction:
//!
//! 1. `sw_alloc(len)` a source buffer, write UTF-8 into wasm memory;
//! 2. `sw_check(src_ptr, src_len, prof_ptr, prof_len)` or
//!    `sw_annotate(src_ptr, src_len)` — both return `0` and leave the envelope in
//!    a thread-local result buffer (a wasm instance is one thread; the
//!    thread-local is just the idiomatic non-`static mut` spelling);
//! 3. `sw_result_ptr()` / `sw_result_len()` to read it;
//! 4. `sw_dealloc` the source buffer.
//!
//! Every envelope carries `"ok"`: `true` with the analysis payload, `false` with
//! an `"error"` string (an unknown profile — the CLI's exit-2 analogue as data,
//! or invalid UTF-8 input). The call itself never traps on user input: a snippet
//! that PHP would reject parses with recovery and analyzes, exactly as the CLI
//! treats it, and its recovered parse errors are reported in the envelope's
//! `parse_errors` — the playground states what the CLI today keeps silent
//! (the known `parse_errors()`-has-no-consumer gap).
//!
//! The `findings` array mirrors the CLI's `--format json` schema key for key
//! (`id`, `layer`, `level`, `path`, `line`, `column`, `message`, plus the facet
//! key when the id declares one), so a playground reader and a CI reader learn
//! one schema. The pipeline is the CLI's, minus the channels that have no
//! meaning for a pasted snippet: vendor filtering (no layout), `[[policy]]`
//! (no config file), and the baseline (no filesystem). Inline `@steins-ignore`
//! **is** applied — a snippet demonstrating suppression must behave like the
//! real tool, including `suppress.unmatched` anti-rot.

use std::cell::RefCell;
use std::collections::BTreeMap;

use steins_db::{Project, ProjectLayout, SourceFile, SteinsDatabase, parse};
use steins_infer::profile::ProfileConfigs;
use steins_infer::suppress::apply_inline_ignores;
use steins_infer::{NoFold, SOUND_SUBSET_NOTICE, annotate_project, check_project_with_runtime};

/// The diagnostic path a playground snippet analyzes under. One file, one
/// project; the name only has to be stable and self-describing in messages.
const SNIPPET_PATH: &str = "playground.php";

thread_local! {
    /// The last envelope produced by [`sw_check`]/[`sw_annotate`]. A wasm
    /// instance is single-threaded, so one buffer is the whole story; the JS
    /// glue copies it out immediately after the call.
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
/// `ptr` must come from [`sw_alloc`] with the same `len`, and must not be used
/// afterwards.
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
///
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

/// Check `src` under the built-in profile named by `prof` (empty = `default`)
/// and leave the JSON envelope in the result buffer. Always returns `0`; the
/// envelope's `ok` field is the real verdict.
///
/// # Safety
///
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

/// Annotate `src` (the `steins annotate` margin facts) and leave the JSON
/// envelope in the result buffer. Always returns `0`.
///
/// # Safety
///
/// The pointer/length pair must describe readable wasm memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sw_annotate(src_ptr: *const u8, src_len: usize) -> i32 {
    let Ok(source) = (unsafe { read_str(src_ptr, src_len) }) else {
        return set_result(error_envelope("source is not valid UTF-8"));
    };
    set_result(annotate_impl(source))
}

/// The target-agnostic body of [`sw_check`] — also what the native tests pin.
fn check_impl(source: &str, selected: Option<&str>) -> serde_json::Value {
    // No steins.toml in a browser: the profile table is empty, so `selected`
    // resolves against the built-ins alone and an unknown name is the CLI's
    // exit-2 config error, delivered as data.
    let configs = ProfileConfigs(BTreeMap::new());
    let surface = match configs.resolve(selected) {
        Ok(s) => s,
        Err(e) => return error_envelope(&e.to_string()),
    };

    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, SNIPPET_PATH.to_owned(), source.to_owned());
    let project = Project::new(&db, vec![file], ProjectLayout::fallback());
    let mut folder = NoFold;
    // `warning_handler_abort = true` is the CLI's DEFAULT (ADR-0049 §7: a proven
    // E_WARNING is a proven runtime break; only `[runtime] warning-handler =
    // "null"` opts out, and a browser snippet has no steins.toml). Passing false
    // here silently withheld every warning-backed finding — offset.maybe-missing
    // among them — which is how the playground's strict rung first shipped
    // quieter than `steins check --profile strict`.
    let mut findings = check_project_with_runtime(&db, project, &mut folder, true);

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
    let db = SteinsDatabase::default();
    let file = SourceFile::new(&db, SNIPPET_PATH.to_owned(), source.to_owned());
    let project = Project::new(&db, vec![file], ProjectLayout::fallback());
    let mut folder = NoFold;
    let facts = annotate_project(&db, project, file, &mut folder);

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

    /// The four built-ins all resolve; the strict rung is selectable here the
    /// way it is on the CLI.
    #[test]
    fn builtin_profiles_resolve() {
        for p in ["default", "contracts", "throws-direct", "strict"] {
            let v = check_impl("<?php\n", Some(p));
            assert_eq!(v["ok"], true, "{p}");
            assert_eq!(v["profile"], p);
        }
    }

    /// A syntactically broken snippet analyzes with recovery and REPORTS its
    /// parse errors in the envelope — the playground states what the CLI today
    /// keeps silent.
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
}

#[cfg(test)]
mod strict_leg {
    use super::*;

    /// The strict rung through the playground path fires exactly as the CLI
    /// does. This pins the ADR-0049 §7 default: `warning_handler_abort = true`
    /// is the CLI's no-config posture, and passing `false` here withheld every
    /// warning-backed finding (offset.maybe-missing among them) — the playground
    /// must never be quieter than `steins check --profile strict` on the same
    /// snippet.
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
