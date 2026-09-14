//! `mine-resource-params`: build the committed resource-CONSUMER table from a
//! pinned php-src checkout (ADR-0097 §2.5, the parameter twin of
//! `resource_returns.toml`).
//!
//! # What is mined
//!
//! Every `*.stub.php` under the checkout is scanned for a docblock that precedes a
//! **free function**, and each `@param` of that docblock whose type is **exactly**
//! `resource` becomes one `[[param]]` row: the function, the 0-based position, the
//! parameter's name (so a finding can name `$stream` the way PHP's own `TypeError`
//! does), and the stub it was read from. Nothing else is read off the stub — the
//! position's shape (untyped, by value, not variadic, no `null` default) is
//! CHECKED, and a position that fails the check is declined rather than
//! transcribed with a caveat.
//!
//! A `@param` whose type is a union naming `resource` (`resource|string`,
//! `resource|null`, `resource|GdImage`) is declined and listed under `[declined]`
//! with its spelling. The ordinary acceptance relation judges those the day the
//! arms lower; a curated union row would be the parameter refinement ADR-0056
//! §9.6 refuses. Methods are declined outright (§4's function-keyed bound) and
//! counted as `methods_skipped`.
//!
//! # Two kinds of column
//!
//! The columns above are the scan's and are rewritten on every run. Four more —
//! `accepts_closed`, `closes`, `kind`, `probe` — are **curated**: each is a
//! behavioral claim about the position (`get_resource_id` answers on a closed
//! handle; `fclose` leaves its argument closed; `fread` demands an open `stream`)
//! and every one of them is a `php -r` transcript recorded beside it in `probe`.
//! The miner never invents one: it reads the committed table back, carries the
//! curated columns forward by `(function, index)`, and REFUSES to write a table
//! when a curated row no longer has a scanned row behind it — stale evidence is
//! not silently dropped, and not silently kept either.
//!
//! # Usage
//!
//! ```text
//! cargo xtask mine-resource-params [--php-src DIR]
//! ```
//!
//! Default checkout: `~/repo/site/php/php-src`, read-only. Its `HEAD` becomes
//! the pin recorded in `[meta]`. Output:
//! `docs/research/phpsrc-mining/resource_params.toml` (source of record); `cargo
//! xtask gen-catalog` turns it into `resource_params_generated.rs`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::corpus::repo_root;
use crate::mine_function_map::git_head;

/// The committed TOML, read back for its curated columns.
#[derive(serde::Deserialize)]
struct Committed {
    #[serde(default)]
    meta: CommittedMeta,
    #[serde(default)]
    param: Vec<CommittedRow>,
}

#[derive(Default, serde::Deserialize)]
struct CommittedMeta {
    /// The PHP the probes were run on — curated, carried forward verbatim.
    probe_php: Option<String>,
}

#[derive(serde::Deserialize)]
struct CommittedRow {
    function: String,
    index: usize,
    #[serde(default)]
    accepts_closed: bool,
    #[serde(default)]
    closes: bool,
    kind: Option<String>,
    probe: Option<String>,
}

/// The curated half of one row, exactly as the committed table spells it.
#[derive(Default, Clone)]
struct Curated {
    accepts_closed: bool,
    closes: bool,
    kind: Option<String>,
    probe: Option<String>,
}

/// One scanned `(function, position)` — the mechanical half of a row.
struct ScanRow {
    name: String,
    stub: String,
}

/// Everything one scan of the checkout produced.
#[derive(Default)]
struct Scan {
    /// `(lowercased function, index)` → the row.
    rows: BTreeMap<(String, usize), ScanRow>,
    /// Lowercased function → the declined positions, each with its reason.
    declined: BTreeMap<String, Vec<String>>,
    stub_files: usize,
    /// Method positions whose `@param` names `resource` in any form.
    methods_skipped: usize,
    /// `ext/zend_test` positions — the extension exists only as a debug build's
    /// test harness, and no analyzed project loads it (the same exclusion
    /// `resource_returns.toml` makes by hand).
    zend_test_skipped: usize,
    /// Docblocks naming `resource` that no function declaration follows.
    unpaired: usize,
}

/// Entry point for `cargo xtask mine-resource-params`.
pub fn run(php_src: Option<&str>) -> Result<(), String> {
    let root = match php_src {
        Some(p) => PathBuf::from(p),
        None => default_checkout()?,
    };
    if !root.join("Zend/zend_builtin_functions.stub.php").is_file() {
        return Err(format!("{} is not a php-src checkout", root.display()));
    }
    let pin = git_head(&root)?;

    let mut files = Vec::new();
    collect_stubs(&root, &mut files)?;
    files.sort();
    let mut scan = Scan { stub_files: files.len(), ..Scan::default() };
    for file in &files {
        let rel = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(file).map_err(|e| format!("read {}: {e}", file.display()))?;
        scan_stub(&rel, &text, &mut scan)?;
    }
    println!(
        "mine-resource-params: {} stub files, {} rows, {} declined positions, {} method positions skipped, {} zend_test positions skipped, {} unpaired docblocks",
        scan.stub_files,
        scan.rows.len(),
        scan.declined.values().map(Vec::len).sum::<usize>(),
        scan.methods_skipped,
        scan.zend_test_skipped,
        scan.unpaired,
    );

    let dst = repo_root().join("docs/research/phpsrc-mining/resource_params.toml");
    let (probe_php, curated) = read_committed(&dst, &scan)?;
    let toml = render(&pin, probe_php.as_deref(), &scan, &curated);
    std::fs::write(&dst, &toml).map_err(|e| format!("write {}: {e}", dst.display()))?;
    println!("mine-resource-params: wrote {}", dst.display());
    println!("mine-resource-params: now run `cargo xtask gen-catalog`");
    Ok(())
}

/// `~/repo/site/php/php-src`, the owner's read-only working checkout — the same
/// one `resource_returns.toml` was read from.
fn default_checkout() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_owned())?;
    Ok(PathBuf::from(home).join("repo/site/php/php-src"))
}

/// Every `*.stub.php` under `dir`, recursively. `std::fs` only — the checkout is
/// a few thousand directories and a walker dependency buys nothing.
fn collect_stubs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            // `.git` is the one directory worth naming; nothing else is skipped,
            // since a stub in a `tests/` folder is still a stub the pin carries.
            if name == ".git" {
                continue;
            }
            collect_stubs(&path, out)?;
        } else if name.ends_with(".stub.php") {
            out.push(path);
        }
    }
    Ok(())
}

/// Read the committed table back for its curated columns, keyed by
/// `(function, index)`; a curated row the scan no longer produces is an error,
/// never a silent drop.
fn read_committed(
    dst: &Path,
    scan: &Scan,
) -> Result<(Option<String>, BTreeMap<(String, usize), Curated>), String> {
    let Ok(text) = std::fs::read_to_string(dst) else {
        return Ok((None, BTreeMap::new()));
    };
    let doc: Committed = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", dst.display()))?;
    let mut curated = BTreeMap::new();
    let mut orphaned = Vec::new();
    for row in doc.param {
        let key = (row.function.to_ascii_lowercase(), row.index);
        let carries = row.accepts_closed || row.closes || row.kind.is_some() || row.probe.is_some();
        if !carries {
            continue;
        }
        if row.kind.is_some() && row.probe.is_none() {
            return Err(format!(
                "{}: `{}` #{} carries a `kind` with no `probe` — a kind is a measured claim (ADR-0097 §2.5)",
                dst.display(),
                row.function,
                row.index
            ));
        }
        if !scan.rows.contains_key(&key) {
            orphaned.push(format!("{} #{}", row.function, row.index));
            continue;
        }
        curated.insert(
            key,
            Curated { accepts_closed: row.accepts_closed, closes: row.closes, kind: row.kind, probe: row.probe },
        );
    }
    if !orphaned.is_empty() {
        return Err(format!(
            "{}: curated evidence for {} no longer has a scanned row behind it — the stub moved; \
             re-probe or remove the row before regenerating",
            dst.display(),
            orphaned.join(", ")
        ));
    }
    Ok((doc.meta.probe_php, curated))
}

// ---------------------------------------------------------------------------
// The stub scanner.
// ---------------------------------------------------------------------------

/// One `@param` line of a docblock: the type as spelled and the bare name.
struct DocParam {
    ty: String,
    name: String,
}

/// One position of a parsed signature.
struct SigParam {
    name: String,
    native_ty: String,
    by_ref: bool,
    variadic: bool,
    default: Option<String>,
}

/// What follows a docblock.
enum Decl {
    Function { name: String, params: Vec<SigParam> },
    Method,
    Other,
}

/// Scan one stub's text into `scan`. `rel` is the checkout-relative path.
fn scan_stub(rel: &str, text: &str, scan: &mut Scan) -> Result<(), String> {
    let lines: Vec<&str> = text.lines().collect();
    let zend_test = rel.starts_with("ext/zend_test/");
    let mut namespace = String::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(ns) = namespace_decl(line) {
            namespace = ns;
        }
        if !line.starts_with("/**") {
            i += 1;
            continue;
        }
        // Collect the docblock.
        let start = i;
        while i < lines.len() && !lines[i].contains("*/") {
            i += 1;
        }
        let end = i.min(lines.len() - 1);
        i += 1;
        let params: Vec<DocParam> = lines[start..=end].iter().filter_map(|l| doc_param(l)).collect();
        let naming_resource: Vec<&DocParam> = params.iter().filter(|p| names_resource(&p.ty)).collect();
        if naming_resource.is_empty() {
            continue;
        }
        // The declaration the docblock precedes: blank lines and attributes may
        // sit between them, nothing else.
        let mut j = i;
        while j < lines.len() && (lines[j].trim().is_empty() || lines[j].trim().starts_with("#[")) {
            j += 1;
        }
        let decl = if j < lines.len() { parse_decl(&lines[j..]) } else { Decl::Other };
        let (fn_name, sig) = match decl {
            Decl::Function { name, params } => (name, params),
            Decl::Method => {
                scan.methods_skipped += naming_resource.len();
                continue;
            }
            Decl::Other => {
                scan.unpaired += 1;
                continue;
            }
        };
        if zend_test {
            scan.zend_test_skipped += naming_resource.len();
            continue;
        }
        let key_fn = if namespace.is_empty() {
            fn_name.to_ascii_lowercase()
        } else {
            format!("{}\\{}", namespace.to_ascii_lowercase(), fn_name.to_ascii_lowercase())
        };
        for dp in naming_resource {
            let Some(index) = sig.iter().position(|p| p.name == dp.name) else {
                return Err(format!(
                    "{rel}: `@param {} ${}` on `{fn_name}` names no parameter of its signature",
                    dp.ty, dp.name
                ));
            };
            let sp = &sig[index];
            let spelled = format!("{} ${}", dp.ty, dp.name);
            let reason = if dp.ty != "resource" {
                Some("a union — judged by the ordinary relation once the arms lower".to_owned())
            } else if sp.by_ref {
                Some("by reference — an out-parameter takes a variable, not a value".to_owned())
            } else if sp.variadic {
                Some("variadic — one parameter binds many arguments".to_owned())
            } else if !sp.native_ty.is_empty() {
                Some(format!("natively `{}` — the engine speaks for this position", sp.native_ty))
            } else if sp.default.as_deref().is_some_and(|d| d.eq_ignore_ascii_case("null")) {
                Some("defaults to null — the position accepts null whatever the docblock says".to_owned())
            } else {
                None
            };
            match reason {
                Some(reason) => {
                    scan.declined.entry(key_fn.clone()).or_default().push(format!("{spelled} — {reason}"));
                }
                None => {
                    let row = ScanRow { name: dp.name.clone(), stub: rel.to_owned() };
                    // `#if`/`#else` alternates may declare a name twice; two
                    // readings that agree are one row, two that differ are a bug.
                    if let Some(prev) = scan.rows.get(&(key_fn.clone(), index)) {
                        if prev.name != row.name {
                            return Err(format!(
                                "{rel}: `{fn_name}` #{index} read as `${}` and `${}`",
                                prev.name, row.name
                            ));
                        }
                    } else {
                        scan.rows.insert((key_fn.clone(), index), row);
                    }
                }
            }
        }
    }
    Ok(())
}

/// `namespace Foo;` / `namespace Foo {` → `Some("Foo")`; the global `namespace {`
/// → `Some("")`; anything else `None`.
fn namespace_decl(line: &str) -> Option<String> {
    let rest = line.strip_prefix("namespace")?;
    if !rest.starts_with(char::is_whitespace) && !rest.starts_with('{') {
        return None;
    }
    let rest = rest.trim();
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '\\').collect();
    let after = rest[name.len()..].trim_start();
    (after.starts_with(';') || after.starts_with('{')).then_some(name)
}

/// The `@param <type> $<name>` of one docblock line, if it carries one.
fn doc_param(line: &str) -> Option<DocParam> {
    let body = line.trim().trim_start_matches("/**").trim_start_matches('*').trim();
    let rest = body.strip_prefix("@param")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim();
    let dollar = rest.find('$')?;
    let ty = rest[..dollar].trim().trim_end_matches("...").trim_end_matches('&').trim().to_owned();
    let name: String = rest[dollar + 1..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if ty.is_empty() || name.is_empty() {
        return None;
    }
    Some(DocParam { ty, name })
}

/// Whether a docblock type spelling names `resource` as itself or as a union arm.
fn names_resource(ty: &str) -> bool {
    ty.split('|').any(|arm| arm.trim().trim_start_matches('?') == "resource")
}

/// Classify and parse the declaration starting at `lines[0]`.
fn parse_decl(lines: &[&str]) -> Decl {
    let head = lines[0].trim();
    let Some(fn_at) = find_keyword(head, "function") else { return Decl::Other };
    let before = head[..fn_at].trim();
    if !before.is_empty() {
        // `public function`, `static function`, … — a method. A free function has
        // nothing before the keyword.
        let modifiers = ["public", "protected", "private", "static", "final", "abstract"];
        return if before.split_whitespace().all(|w| modifiers.contains(&w)) { Decl::Method } else { Decl::Other };
    }
    let after = head[fn_at + "function".len()..].trim_start().trim_start_matches('&').trim_start();
    let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if name.is_empty() {
        return Decl::Other;
    }
    // The parameter list, `(` to its matching `)`, across as many lines as it takes.
    let mut text = String::new();
    let mut depth = 0i32;
    let mut started = false;
    'outer: for (n, l) in lines.iter().enumerate() {
        let l = if n == 0 { &head[fn_at..] } else { l };
        let mut quote: Option<char> = None;
        let mut chars = l.chars().peekable();
        while let Some(c) = chars.next() {
            if let Some(q) = quote {
                text.push(c);
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        text.push(next);
                    }
                } else if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '\'' | '"' if started => {
                    quote = Some(c);
                    text.push(c);
                }
                '(' => {
                    depth += 1;
                    if started {
                        text.push(c);
                    }
                    started = true;
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break 'outer;
                    }
                    text.push(c);
                }
                _ if started => text.push(c),
                _ => {}
            }
        }
        text.push(' ');
    }
    Decl::Function { name, params: split_params(&text).iter().filter_map(|p| parse_param(p)).collect() }
}

/// The byte offset of `word` in `s` as a whole word, if present.
fn find_keyword(s: &str, word: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = s[from..].find(word) {
        let at = from + at;
        let before_ok = at == 0 || !s[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
        let after = &s[at + word.len()..];
        let after_ok = after.is_empty() || !after.starts_with(|c: char| c.is_alphanumeric() || c == '_');
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + word.len();
    }
    None
}

/// Split a parameter list at its depth-0 commas.
fn split_params(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            cur.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                cur.push(c);
            }
            '(' | '[' | '{' | '<' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' | '>' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// One parameter's text (`#[\SensitiveParameter] string $password = ""`,
/// `mixed &...$vars`, `$stream`) → its shape.
fn parse_param(text: &str) -> Option<SigParam> {
    let mut decl = text.trim();
    while let Some(rest) = decl.strip_prefix("#[") {
        let close = rest.find(']')?;
        decl = rest[close + 1..].trim();
    }
    let (decl, default) = match decl.find('=') {
        Some(eq) => (decl[..eq].trim(), Some(decl[eq + 1..].trim().to_owned())),
        None => (decl, None),
    };
    let dollar = decl.find('$')?;
    let before = &decl[..dollar];
    let name: String = decl[dollar + 1..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if name.is_empty() {
        return None;
    }
    let by_ref = before.contains('&');
    let variadic = before.contains("...");
    let native_ty = before.replace("...", " ").replace('&', " ").trim().to_owned();
    Some(SigParam { name, native_ty, by_ref, variadic, default })
}

// ---------------------------------------------------------------------------
// The TOML.
// ---------------------------------------------------------------------------

/// A TOML-safe quoted key or string.
fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Render the committed source of record.
fn render(
    pin: &str,
    probe_php: Option<&str>,
    scan: &Scan,
    curated: &BTreeMap<(String, usize), Curated>,
) -> String {
    let mut s = String::new();
    s.push_str(
        "# resource_params.toml — builtin POSITIONS that demand a legacy PHP RESOURCE\n\
         # (ADR-0097 §2.5, the parameter twin of resource_returns.toml).\n\
         #\n\
         # SOURCE OF RECORD. Generated by `cargo xtask mine-resource-params`, which scans\n\
         # every `*.stub.php` of a pinned php-src checkout for a docblock `@param` whose\n\
         # type is EXACTLY `resource` on a free function. Regenerate alongside a\n\
         # `PINNED_PHP` bump, the way resource_returns.toml is re-read — never by hand.\n\
         # `cargo xtask gen-catalog` emits crates/steins-catalog/src/resource_params_generated.rs.\n\
         #\n\
         # ─────────────────── Why a table, and why the stubs ───────────────────\n\
         # Every other builtin parameter is judged against the REFLECTED signature — the\n\
         # running engine's own `getParameters()` (ADR-0056 §9). PHP has no syntax for a\n\
         # `resource` parameter, so reflection reports NO type at every one of these\n\
         # positions, and the engine checks at the call instead: `fwrite('x', …)` is\n\
         # `TypeError: fwrite(): Argument #1 ($stream) must be of type resource, string\n\
         # given`, in either coercion mode. The stub docblock is the only place the\n\
         # demand is written down, and this table is that reading at the pin.\n\
         #\n\
         # ADMISSION is ADR-0056 §8.2's gate in the parameter direction, all three\n\
         # conditions checked at the call site and none of them here:\n\
         #   1. the stub at the pin says `resource` (this table IS that reading);\n\
         #   2. the analyzing engine declares NO type at the position — THE TRIPWIRE:\n\
         #      the day `fwrite` declares `Stream $stream`, the engine speaks and the\n\
         #      row is disowned, with no re-mining and no staleness window;\n\
         #   3. the project PHP minor equals `steins_catalog::PINNED_PHP`.\n\
         #\n\
         # ─────────────────── Two kinds of column ───────────────────\n\
         # MECHANICAL (the scan's, rewritten on every run): `function` (lowercased;\n\
         # namespaced names keep their `\\`), `index` (0-based position), `name` (the\n\
         # `$param`, so a finding can name it the way PHP's TypeError does), `stub`.\n\
         # A position is a row only when the stub declares it untyped, by value, not\n\
         # variadic, and with no `null` default; anything else is DECLINED below.\n\
         #\n\
         # CURATED (carried forward from the committed file by (function, index), never\n\
         # invented, refused when orphaned): `accepts_closed` (the position answers on a\n\
         # closed handle; default false), `closes` (the call leaves its argument closed;\n\
         # default false), `kind` (the resource kind the position demands — `stream`,\n\
         # `dir`, `process`, `stream-context`; absent unless probed), `probe` (the\n\
         # verbatim `php -r` transcript that established the curated bits). Nothing is\n\
         # recalled from memory: a row with a `kind` carries its probe, and so does every\n\
         # `true` bit. A row with no probe carries only the defaults, which are the\n\
         # stub's claim and not a measurement.\n\
         #\n\
         # ─────────────────── What is deliberately NOT here ───────────────────\n\
         # * Union positions (`resource|string`, `resource|null`, `resource|GdImage`):\n\
         #   listed under [declined] with their spelling. The ordinary relation judges\n\
         #   them once the arms lower; a curated union row would be the parameter\n\
         #   refinement ADR-0056 §9.6 refuses.\n\
         # * Method positions (`SplFileObject::__construct`'s `resource|string`,\n\
         #   `Phar::setStub`): ADR-0056 §4's function-keyed bound, counted as\n\
         #   `methods_skipped`.\n\
         # * `ext/zend_test` positions: the extension is a debug build's harness and no\n\
         #   analyzed project loads it — the same exclusion resource_returns.toml makes.\n\n",
    );
    let _ = writeln!(s, "[meta]");
    let _ = writeln!(s, "php_src_commit = {}", toml_str(pin));
    if let Some(v) = probe_php {
        let _ = writeln!(s, "probe_php = {}", toml_str(v));
    }
    let _ = writeln!(s, "generator = \"cargo xtask mine-resource-params\"");
    let _ = writeln!(
        s,
        "stub_scan = \"docblock `@param` of exactly `resource` on a free function's untyped, by-value, non-variadic position with no null default\""
    );
    let _ = writeln!(s, "stub_files = {}", scan.stub_files);
    let _ = writeln!(s, "rows = {}", scan.rows.len());
    let _ = writeln!(s, "declined = {}", scan.declined.values().map(Vec::len).sum::<usize>());
    let _ = writeln!(s, "methods_skipped = {}", scan.methods_skipped);
    let _ = writeln!(s, "zend_test_skipped = {}", scan.zend_test_skipped);
    let _ = writeln!(s, "unpaired_docblocks = {}", scan.unpaired);
    let _ = writeln!(s, "probed = {}", curated.values().filter(|c| c.probe.is_some()).count());
    s.push('\n');

    s.push_str(
        "# Positions the scan saw and refused, verbatim, with the reason: a union naming\n\
         # `resource`, a by-reference or variadic position, a position the stub types\n\
         # natively, or one whose default is `null`. Each is silence at the call site.\n",
    );
    let _ = writeln!(s, "[declined]");
    for (name, entries) in &scan.declined {
        let items: Vec<String> = entries.iter().map(|e| toml_str(e)).collect();
        let _ = writeln!(s, "{} = [{}]", toml_str(name), items.join(", "));
    }
    s.push('\n');

    for ((function, index), row) in &scan.rows {
        let c = curated.get(&(function.clone(), *index)).cloned().unwrap_or_default();
        let _ = writeln!(s, "[[param]]");
        let _ = writeln!(s, "function = {}", toml_str(function));
        let _ = writeln!(s, "index = {index}");
        let _ = writeln!(s, "name = {}", toml_str(&row.name));
        let _ = writeln!(s, "stub = {}", toml_str(&row.stub));
        let _ = writeln!(s, "accepts_closed = {}", c.accepts_closed);
        let _ = writeln!(s, "closes = {}", c.closes);
        if let Some(kind) = &c.kind {
            let _ = writeln!(s, "kind = {}", toml_str(kind));
        }
        if let Some(probe) = &c.probe {
            let _ = writeln!(s, "probe = {}", toml_str(probe));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(text: &str) -> Scan {
        let mut scan = Scan::default();
        scan_stub("ext/x/x.stub.php", text, &mut scan).expect("scans");
        scan
    }

    #[test]
    fn an_exact_resource_param_on_a_free_function_is_a_row() {
        let s = scan("<?php\n/** @param resource $stream */\nfunction fwrite($stream, string $data, ?int $length = null): int|false {}\n");
        let row = &s.rows[&("fwrite".to_owned(), 0)];
        assert_eq!(row.name, "stream");
        assert_eq!(row.stub, "ext/x/x.stub.php");
        assert!(s.declined.is_empty());
    }

    #[test]
    fn a_multi_line_docblock_names_every_position() {
        let s = scan(
            "<?php\n/**\n * @param resource $from\n * @param resource $to\n * @param resource|null $context\n */\n\
             function stream_copy_to_stream($from, $to, ?int $length = null, int $offset = 0, $context = null): int|false {}\n",
        );
        assert!(s.rows.contains_key(&("stream_copy_to_stream".to_owned(), 0)));
        assert!(s.rows.contains_key(&("stream_copy_to_stream".to_owned(), 1)));
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.declined["stream_copy_to_stream"].len(), 1);
        assert!(s.declined["stream_copy_to_stream"][0].starts_with("resource|null $context — a union"));
    }

    #[test]
    fn unions_by_ref_variadics_native_types_and_null_defaults_are_declined() {
        let s = scan(
            "<?php\n/** @param resource|string $file */\nfunction a($file) {}\n\
             /** @param resource $out */\nfunction b(&$out) {}\n\
             /** @param resource $many */\nfunction c(...$many) {}\n\
             /** @param resource $typed */\nfunction d(mixed $typed) {}\n\
             /** @param resource $ctx */\nfunction e($ctx = null) {}\n",
        );
        assert!(s.rows.is_empty(), "{:?}", s.rows.keys().collect::<Vec<_>>());
        assert_eq!(s.declined.len(), 5);
        assert!(s.declined["b"][0].contains("by reference"));
        assert!(s.declined["c"][0].contains("variadic"));
        assert!(s.declined["d"][0].contains("natively `mixed`"));
        assert!(s.declined["e"][0].contains("defaults to null"));
    }

    #[test]
    fn methods_are_counted_and_not_rows() {
        let s = scan(
            "<?php\nclass C {\n    /** @param resource $stream */\n    public function __construct($stream) {}\n\
             \x20   /** @param resource|null $context */\n    public static function open($context = null) {}\n}\n",
        );
        assert!(s.rows.is_empty());
        assert_eq!(s.methods_skipped, 2);
    }

    #[test]
    fn attributes_and_a_signature_across_lines_are_read_through() {
        let s = scan(
            "<?php\n/**\n * @param resource $zip\n */\n#[\\Deprecated(since: '8.0', message: 'use ZipArchive::close() instead')]\n\
             function zip_close(\n    $zip,\n    #[\\SensitiveParameter] string $secret = \"a,b)\"\n): void {}\n",
        );
        assert!(s.rows.contains_key(&("zip_close".to_owned(), 0)));
    }

    #[test]
    fn a_namespaced_function_keeps_its_namespace_and_zend_test_is_skipped() {
        let s = scan("<?php\nnamespace Foo {\n    /** @param resource $s */\n    function bar($s) {}\n}\n");
        assert!(s.rows.contains_key(&("foo\\bar".to_owned(), 0)));
        let mut z = Scan::default();
        scan_stub("ext/zend_test/test.stub.php", "<?php\n/** @param resource $s */\nfunction zend_test_cast_fread($s) {}\n", &mut z)
            .expect("scans");
        assert!(z.rows.is_empty());
        assert_eq!(z.zend_test_skipped, 1);
    }

    #[test]
    fn a_docblock_no_function_follows_is_unpaired() {
        let s = scan("<?php\n/** @param resource $s */\nconst X = 1;\n");
        assert!(s.rows.is_empty());
        assert_eq!(s.unpaired, 1);
    }
}
