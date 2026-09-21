//! ADR-0097 §2.7 — the folds that come with the kind: `gettype`,
//! `get_debug_type`, `get_resource_type` and `get_resource_id` over a handle the
//! walk has proven. (`is_resource`, the fifth, is a guard and lives in
//! `resource_values.rs`.)
//!
//! These folds feed narrowing and value facts, not a diagnostic id of their own,
//! so the failure mode is not a finding that fires: it is a **wrong string**
//! that some later judgment convicts on, and a guard over the folded value that
//! the walk then calls dead. What is pinned here is therefore both halves —
//! what each fold answers, and what the answer does one guard later — plus every
//! way the proof can be missing, each of which must leave exactly today's
//! declared answer behind.
//!
//! Every runtime claim was probed at 8.5.10 (the catalog pin); the transcripts
//! live on `resource_folds.rs`'s tables. Two of them are worth naming here,
//! because they contradict what §2.7 was drafted with:
//!
//! * `get_debug_type()` of a CLOSED handle is `'resource (closed)'`, not the
//!   kind spelling — the kind is gone the moment the handle is;
//! * a handle's `Open` state is a proof only for the producers whose handle
//!   nothing else can close. A stream filter dies with its stream, a
//!   `proc_open` pipe with its process, and `pfsockopen` hands the SAME handle
//!   back twice — so those fold from `Unknown` even when nothing named them.

use std::collections::HashMap;

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, EngineFolder, FoldEngine, check_with};
use steins_sidecar::{
    BuiltinParam, ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile,
    Reflection,
};
use steins_syntax::SourceTree;

// ---------------------------------------------------------------------------
// Mock engine: what `ReflectionFunction` answers at 8.5.10 for these names
// ---------------------------------------------------------------------------

fn p(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam {
        name: name.to_owned(),
        ty: ty.map(ToOwned::to_owned),
        by_ref: false,
        variadic: false,
        optional: false,
    }
}

fn optional(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam { optional: true, ..p(name, ty) }
}

fn by_ref(name: &str) -> BuiltinParam {
    BuiltinParam { by_ref: true, ..p(name, None) }
}

struct Engine {
    params: HashMap<String, Vec<BuiltinParam>>,
    returns: HashMap<String, String>,
    absent: Vec<String>,
    version: String,
}

impl Engine {
    /// The four folds declare `string`/`string`/`string`/`int`, one required
    /// parameter each; the producers and closers declare no return type, which
    /// is what keeps their rows admitted.
    fn pinned() -> Self {
        let mut params = HashMap::new();
        for (name, arg) in [
            ("gettype", "value"),
            ("get_debug_type", "value"),
            ("get_resource_type", "resource"),
            ("get_resource_id", "resource"),
        ] {
            params.insert(name.to_owned(), vec![p(arg, None)]);
        }
        params.insert(
            "fopen".to_owned(),
            vec![
                p("filename", Some("string")),
                p("mode", Some("string")),
                optional("use_include_path", Some("bool")),
                optional("context", None),
            ],
        );
        params.insert("opendir".to_owned(), vec![p("directory", Some("string"))]);
        params.insert("tmpfile".to_owned(), Vec::new());
        params.insert("stream_context_create".to_owned(), vec![optional("options", Some("?array"))]);
        params.insert(
            "stream_filter_append".to_owned(),
            vec![p("stream", None), p("filter_name", Some("string"))],
        );
        params.insert(
            "proc_open".to_owned(),
            vec![
                p("command", Some("array|string")),
                p("descriptor_spec", Some("array")),
                by_ref("pipes"),
            ],
        );
        params.insert("fclose".to_owned(), vec![p("stream", None)]);
        params.insert("closedir".to_owned(), vec![p("dir_handle", None)]);
        params.insert("proc_close".to_owned(), vec![p("process", None)]);
        params.insert("ftell".to_owned(), vec![p("stream", None)]);
        params.insert("strlen".to_owned(), vec![p("string", Some("string"))]);
        params.insert("intdiv".to_owned(), vec![p("num1", Some("int")), p("num2", Some("int"))]);
        let mut returns = HashMap::new();
        for (name, ty) in [
            ("gettype", "string"),
            ("get_debug_type", "string"),
            ("get_resource_type", "string"),
            ("get_resource_id", "int"),
            ("strlen", "int"),
            ("intdiv", "int"),
            ("ftell", "int|false"),
            ("fclose", "bool"),
            ("closedir", "void"),
            ("proc_close", "int"),
        ] {
            returns.insert(name.to_owned(), ty.to_owned());
        }
        Engine { params, returns, absent: Vec::new(), version: "8.5.10".to_owned() }
    }

    fn returning(mut self, name: &str, ty: &str) -> Self {
        self.returns.insert(name.to_owned(), ty.to_owned());
        self
    }

    /// A signature that moved under a rule that reads argument 0 positionally.
    fn taking(mut self, name: &str, params: Vec<BuiltinParam>) -> Self {
        self.params.insert(name.to_owned(), params);
        self
    }

    fn without(mut self, name: &str) -> Self {
        self.absent.push(name.to_owned());
        self
    }

    fn on_php(mut self, version: &str) -> Self {
        self.version = version.to_owned();
        self
    }
}

impl FoldEngine for Engine {
    fn env(&mut self) -> Option<EnvInfo> {
        Some(EnvInfo {
            php_version: self.version.clone(),
            extensions: vec!["Core".to_owned(), "standard".to_owned()],
            sapi: "cli".to_owned(),
            int_size: Some(8),
        })
    }
    fn reflect(&mut self, target: &str) -> Option<Reflection> {
        let key = target.to_ascii_lowercase();
        let known = self.params.contains_key(&key) && !self.absent.contains(&key);
        let params = known.then(|| self.params[&key].clone());
        Some(Reflection {
            target: target.to_owned(),
            function_exists: known,
            class_like_exists: false,
            return_type: known.then(|| self.returns.get(&key).cloned()).flatten(),
            return_type_tentative: false,
            params_total: params.as_ref().map(|ps| ps.len() as u32),
            params_required: params
                .as_ref()
                .map(|ps| ps.iter().filter(|p| !p.optional).count() as u32),
            params,
        })
    }
    fn reflect_class(&mut self, _target: &str) -> Option<ClassReflection> {
        None
    }
    fn fold(&mut self, _name: &str, _args: &[FoldArg], _strict: bool) -> FoldResult {
        FoldResult::widen("stub")
    }
    fn preg_compile(&mut self, _pattern: &str) -> Option<PregCompile> {
        None
    }
    fn constant_defined(&mut self, _name: &str) -> Option<ConstantDefined> {
        None
    }
    fn restarts(&self) -> u32 {
        0
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn findings(src: &str, engine: Engine, ids: &[&str]) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let mut folder = EngineFolder::with_engine(engine);
    check_with(&tree, &[], "t.php", &mut folder)
        .into_iter()
        .filter(|d: &Diagnostic| ids.contains(&d.id))
        .map(|d: Diagnostic| d.message)
        .collect()
}

/// The `\PHPStan\dumpType()` renderings, in source order, with the label off.
fn dumped(src: &str, engine: Engine) -> Vec<String> {
    findings(src, engine, &[DEBUG_TYPE_ID])
        .into_iter()
        .map(|m| m.trim_start_matches("dumped type: ").to_owned())
        .collect()
}

/// The proof-layer argument mismatches — what a wrong fold would convict on.
fn mismatches(src: &str, engine: Engine) -> Vec<String> {
    findings(src, engine, &[steins_infer::ID])
}

fn phpdoc_mismatches(src: &str, engine: Engine) -> Vec<String> {
    findings(src, engine, &[steins_infer::PARAM_MISMATCH_ID])
}

/// A file-scope handle would be forgotten by any unresolvable call (ADR-0097
/// §2.4's top-level frame row), and `\PHPStan\dumpType()` is one — so every
/// fixture below is a function body.
fn in_fn(body: &str) -> String {
    format!("<?php\nfunction subject(): void {{\n{body}}}\n")
}

/// The handle, guarded: `$h` is a bare `Verified` resource from here on.
const OPEN: &str = "$h = fopen('php://memory', 'r');\nif ($h === false) { return; }\n";

/// Bind each fold's answer to a variable and dump it — the spelling the rung
/// answers at, and the one a later guard reads.
fn folds_of(subject: &str) -> String {
    let mut src = String::new();
    for (i, f) in ["gettype", "get_debug_type", "get_resource_type", "get_resource_id"]
        .iter()
        .enumerate()
    {
        src.push_str(&format!("$f{i} = {f}({subject});\n\\PHPStan\\dumpType($f{i});\n"));
    }
    src
}

// ---------------------------------------------------------------------------
// §2.7 — what each fold answers, by state
// ---------------------------------------------------------------------------

#[test]
fn an_open_handle_folds_to_its_state_and_its_kind() {
    // Probed at 8.5.10 on the handle `fopen('php://memory', 'r')` answers:
    //   gettype()           => "resource"
    //   get_debug_type()    => "resource (stream)"
    //   get_resource_type() => "stream"
    //   get_resource_id()   => 5   (>= 1 for every handle the engine mints)
    assert_eq!(
        dumped(&in_fn(&format!("{OPEN}{}", folds_of("$h"))), Engine::pinned()),
        ["'resource'", "'resource (stream)'", "'stream'", "int<1, max>"],
    );
}

#[test]
fn a_closed_handle_folds_to_the_closed_spellings_and_keeps_its_id() {
    // Probed at 8.5.10 after `fclose($h)`:
    //   gettype()           => "resource (closed)"
    //   get_debug_type()    => "resource (closed)"   <- NOT the kind
    //   get_resource_type() => "Unknown"             <- a different string
    //   get_resource_id()   => 5                      (the same id as when open)
    assert_eq!(
        dumped(&in_fn(&format!("{OPEN}fclose($h);\n{}", folds_of("$h"))), Engine::pinned()),
        ["'resource (closed)'", "'resource (closed)'", "'Unknown'", "int<1, max>"],
    );
}

#[test]
fn an_unknown_state_folds_to_the_union_of_both_states() {
    // An escape into a project call (ADR-0097 §2.4): the handle is a resource —
    // the lane says so — and nothing here knows whether the callee closed it.
    // The union is what BOTH states can answer, which is the honest fold; the
    // id is the one answer the state cannot change.
    let src = in_fn(&format!("{OPEN}helper($h);\n{}", folds_of("$h")));
    let src = format!("{src}function helper($x): void {{}}\n");
    assert_eq!(
        dumped(&src, Engine::pinned()),
        [
            "'resource'|'resource (closed)'",
            "'resource (closed)'|'resource (stream)'",
            "'Unknown'|'stream'",
            "int<1, max>",
        ],
    );
}

#[test]
fn every_probed_kind_folds_to_the_spelling_php_uses() {
    // The kind spellings are PHP's, not Steins' (`ResourceKind::as_str`): a
    // directory handle is a plain `stream` — the `dir` kind exists only for the
    // closing table — and a filter is `stream filter` with a SPACE.
    let cases = [
        ("$h = opendir('.');\nif ($h === false) { return; }\n", "'resource (stream)'", "'stream'"),
        (
            "$h = stream_context_create();\n",
            "'resource (stream-context)'",
            "'stream-context'",
        ),
    ];
    for (producer, debug, kind) in cases {
        let src = in_fn(&format!(
            "{producer}$a = get_debug_type($h);\n\\PHPStan\\dumpType($a);\n\
             $b = get_resource_type($h);\n\\PHPStan\\dumpType($b);\n"
        ));
        assert_eq!(dumped(&src, Engine::pinned()), [debug, kind], "for `{producer}`");
    }
    // The process handle of `proc_open`, whose pipes carry the `stream` spelling
    // under the same producer name.
    let src = in_fn(
        "$p = proc_open('ls', [1 => ['pipe', 'w']], $pipes);\nif ($p === false) { return; }\n\
         $a = get_debug_type($p);\n\\PHPStan\\dumpType($a);\n",
    );
    assert_eq!(dumped(&src, Engine::pinned()), ["'resource (process)'"]);
}

// ---------------------------------------------------------------------------
// The proof, and every way of not having it: today's answer, exactly
// ---------------------------------------------------------------------------

/// What the four folds answer with no proof at all — the declared return of
/// each, which is what every fallback below must still read.
/// `get_debug_type`'s `non-falsy-string` is a curated return-fact row (ADR-0056
/// §1.2): every type keyword it can answer is at least three characters long.
const DECLARED: [&str; 4] = ["string", "non-falsy-string", "string", "int"];

#[test]
fn an_undischarged_false_arm_folds_nothing() {
    // `resource|false` is two arms, so §8.6's lock is shut and the fold has no
    // subject — the same clause the argument families stand on.
    let src = in_fn(&format!(
        "$h = fopen('php://memory', 'r');\n{}",
        folds_of("$h")
    ));
    assert_eq!(dumped(&src, Engine::pinned()), DECLARED);
}

#[test]
fn a_value_that_is_not_a_handle_folds_nothing() {
    let src = in_fn(&format!("$h = 'x';\n{}", folds_of("$h")));
    assert_eq!(dumped(&src, Engine::pinned()), DECLARED);
}

#[test]
fn a_migrated_producer_folds_nothing() {
    // §8.2's tripwire: an engine that declares a return type has disowned the
    // producer row, so there is no proven handle to fold over.
    let src = in_fn(&format!("{OPEN}{}", folds_of("$h")));
    assert_eq!(dumped(&src, Engine::pinned().returning("fopen", "SplFileObject|false")), DECLARED);
}

#[test]
fn a_php_off_the_catalog_pin_folds_nothing() {
    let src = in_fn(&format!("{OPEN}{}", folds_of("$h")));
    // Off the pin `get_debug_type`'s curated row is refused too (ADR-0056 §2),
    // so this one reads the bare reflected envelope rather than [`DECLARED`].
    assert_eq!(
        dumped(&src, Engine::pinned().on_php("8.4.12")),
        ["string", "string", "string", "int"],
    );
}

#[test]
fn a_fold_the_engine_no_longer_declares_the_same_way_withholds() {
    // ADR-0061 §2, both legs: the declaration the rule was written against, and
    // the arity it reads argument 0 under. An engine that moved either one gets
    // the rule withheld rather than demoted — and an absent name says nothing at
    // all, which is the declared floor.
    let src = in_fn(&format!("{OPEN}$a = gettype($h);\n\\PHPStan\\dumpType($a);\n"));
    // A declaration the rule was not written against: the engine's own `mixed`
    // seeds nothing representable, so the answer drops to the catalog floor.
    assert_eq!(dumped(&src, Engine::pinned().returning("gettype", "mixed")), ["string (asserted)"]);
    // An arity that moved, with the declaration unchanged: the declared
    // envelope, exactly as before this rung existed.
    assert_eq!(
        dumped(
            &src,
            Engine::pinned().taking("gettype", vec![p("flags", Some("int")), p("value", None)]),
        ),
        ["string"],
    );
    // A name this engine does not have: the catalog floor speaks, as it does for
    // every builtin an unloaded extension takes with it.
    assert_eq!(dumped(&src, Engine::pinned().without("gettype")), ["string (asserted)"]);
}

#[test]
fn a_project_function_shadowing_the_fold_folds_nothing() {
    let src = format!(
        "<?php\nfunction gettype($v): string {{ return 'x'; }}\n\
         function subject(): void {{\n{OPEN}$a = gettype($h);\n\\PHPStan\\dumpType($a);\n}}\n"
    );
    // The project's own `gettype` is what runs, and this walk knows nothing
    // about its result — `unknown`, not the builtin's `string`.
    assert_eq!(dumped(&src, Engine::pinned()), ["unknown"]);
}

#[test]
fn a_composed_spelling_keeps_the_declared_answer() {
    // The state is read on the store the statement STARTED with, and in
    // `fclose($h) . gettype($h)` the close has already run when `gettype` reads
    // the handle. Every spelling but "the right-hand side is the call" therefore
    // keeps the declared answer — two answers for one value, and the weaker one
    // is the sound one here.
    for (rhs, declared) in [
        ("fclose($h) . gettype($h)", "string"),
        ("gettype($h) . 'x'", "non-falsy-string"),
        ("(string) gettype($h)", "string"),
        // A ternary whose arms the walk cannot resolve binds nothing at all —
        // which is what the fold must not change either.
        ("true ? gettype($h) : 'x'", "unknown"),
    ] {
        let src = in_fn(&format!("{OPEN}$a = {rhs};\n\\PHPStan\\dumpType($a);\n"));
        assert_eq!(dumped(&src, Engine::pinned()), [declared], "for `{rhs}`");
    }
    // A dump with a second argument is the same hazard: the other argument may
    // be a call that runs first.
    let src = in_fn(&format!("{OPEN}\\PHPStan\\dumpType(fclose($h), gettype($h));\n"));
    assert_eq!(dumped(&src, Engine::pinned()), ["bool", "string"]);
}

#[test]
fn the_call_spelling_and_the_binding_spelling_agree() {
    // One value, two spellings, one answer (issue #646's rule) — for the one
    // dump shape where nothing else of the statement runs first.
    // In two scopes, because a dump call is itself a call the walk cannot
    // resolve, and a handle named inside its argument escapes (ADR-0097 §2.4)
    // exactly as it would into any project call.
    let src = format!(
        "<?php\nfunction a(): void {{\n{OPEN}\\PHPStan\\dumpType(gettype($h));\n}}\n\
         function b(): void {{\n{OPEN}$a = gettype($h);\n\\PHPStan\\dumpType($a);\n}}\n"
    );
    assert_eq!(dumped(&src, Engine::pinned()), ["'resource'", "'resource'"]);
}

// ---------------------------------------------------------------------------
// Places (ADR-0098 §2.2): the fold reaches an element, under a proven key
// ---------------------------------------------------------------------------

#[test]
fn a_fold_reaches_an_element_place_under_a_proven_key() {
    // `$pipes[0]` is a handle from the moment `proc_open` writes it, and the
    // fold reads it through the same `place_of` the closed-state argument cell
    // reads. A pipe is not `Open`-provable (its process can close it), so the
    // state half is the union — the KEY half is what this pins.
    let pipes = "$p = proc_open('ls', [0 => ['pipe', 'r']], $pipes);\nif ($p === false) { return; }\n";
    for subject in ["$pipes[0]", "$pipes[$i]"] {
        let src = in_fn(&format!(
            "{pipes}$i = 0;\n$a = gettype({subject});\n\\PHPStan\\dumpType($a);\n"
        ));
        assert_eq!(
            dumped(&src, Engine::pinned()),
            ["'resource'|'resource (closed)'"],
            "for `{subject}`",
        );
    }
    // A closed element folds to the closed spelling, by the same place.
    let src = in_fn(&format!(
        "{pipes}fclose($pipes[0]);\n$a = gettype($pipes[0]);\n\\PHPStan\\dumpType($a);\n"
    ));
    assert_eq!(dumped(&src, Engine::pinned()), ["'resource (closed)'"]);
    // An array literal over a bound handle binds the same allocation (ADR-0098
    // §2.2) and the fold reaches it — at the state that assignment left it in,
    // which is `Unknown`: storing a handle into an array is an escape (§2.4).
    // The close below is what sharpens it, through the same place.
    let src = in_fn(&format!(
        "{OPEN}$bag = [$h];\n$a = gettype($bag[0]);\n\\PHPStan\\dumpType($a);\n"
    ));
    assert_eq!(dumped(&src, Engine::pinned()), ["'resource'|'resource (closed)'"]);
    let src = in_fn(&format!(
        "{OPEN}$bag = [$h];\nfclose($bag[0]);\n\
         $a = gettype($bag[0]);\n\\PHPStan\\dumpType($a);\n"
    ));
    assert_eq!(dumped(&src, Engine::pinned()), ["'resource (closed)'"]);
}

#[test]
fn a_key_that_could_run_code_names_no_place() {
    // `place_of` resolves a key through a project call, and at top level such a
    // call can rebind the base before the fold's own call runs. A key spelled as
    // anything but a literal or a variable therefore names nothing here.
    let src = format!(
        "<?php\nfunction two(): int {{ return 0; }}\n\
         function subject(): void {{\n{OPEN}$bag = [$h];\n\
         $a = gettype($bag[two()]);\n\\PHPStan\\dumpType($a);\n}}\n"
    );
    assert_eq!(dumped(&src, Engine::pinned()), ["string"]);
    // A key the walk cannot prove at all names nothing either, as everywhere.
    let src = in_fn(&format!(
        "{OPEN}$bag = [$h];\n$a = gettype($bag[$k]);\n\\PHPStan\\dumpType($a);\n"
    ));
    assert_eq!(dumped(&src, Engine::pinned()), ["string"]);
}

// ---------------------------------------------------------------------------
// `Open` is a proof only where the producer can vouch for it
// ---------------------------------------------------------------------------

#[test]
fn a_handle_another_handle_can_close_never_folds_to_open() {
    // Probed at 8.5.10, each of these reads `resource (closed)` with nothing
    // having named it:
    //   $f = stream_filter_append(fopen('php://memory','r'), 'string.rot13');
    //       -> the stream's last reference is gone, so the FILTER is closed
    //   proc_open(…, $pipes); proc_close($p);      -> every pipe is closed
    //   $a = pfsockopen(…); $b = pfsockopen(…);    -> ONE handle, two names
    // So `Open` is not a proof for them, and both state-sensitive folds answer
    // the union even on a handle this walk has only just seen produced.
    let src = in_fn(
        "$m = fopen('php://memory', 'r');\nif ($m === false) { return; }\n\
         $f = stream_filter_append($m, 'string.rot13');\nif ($f === false) { return; }\n\
         $a = gettype($f);\n\\PHPStan\\dumpType($a);\n\
         $b = get_resource_type($f);\n\\PHPStan\\dumpType($b);\n",
    );
    assert_eq!(
        dumped(&src, Engine::pinned()),
        ["'resource'|'resource (closed)'", "'Unknown'|'stream filter'"],
    );
}

// ---------------------------------------------------------------------------
// What the fold's answer does one guard later
// ---------------------------------------------------------------------------

/// A guard over the folded value, with a finding inside it: the finding is the
/// evidence that the branch is live, its absence that the branch is dead.
fn guard(prelude: &str, cond: &str) -> String {
    in_fn(&format!("{OPEN}{prelude}$t = gettype($h);\nif ({cond}) {{ intdiv('a', 1); }}\n"))
}

#[test]
fn a_guard_on_the_folded_value_stays_live_where_the_string_can_occur() {
    let src = guard("fclose($h);\n", "$t === 'resource (closed)'");
    assert_eq!(mismatches(&src, Engine::pinned()).len(), 1, "the closed branch must stay live");
    let src = guard("", "$t === 'resource'");
    assert_eq!(mismatches(&src, Engine::pinned()).len(), 1, "the open branch must stay live");
}

#[test]
fn a_guard_the_fold_refutes_is_dead() {
    // The other half, and the one a WRONG fold would produce on live code: with
    // `gettype` answering `string`, both of these branches were live.
    let src = guard("fclose($h);\n", "$t === 'resource'");
    assert!(mismatches(&src, Engine::pinned()).is_empty(), "a closed handle is never 'resource'");
    let src = guard("", "$t === 'resource (closed)'");
    assert!(mismatches(&src, Engine::pinned()).is_empty(), "an open handle is never closed");
    // An unknown state refutes neither: the union keeps both branches alive.
    let unknown = format!(
        "{}function helper($x): void {{}}\n",
        in_fn(&format!(
            "{OPEN}helper($h);\n$t = gettype($h);\n\
             if ($t === 'resource (closed)') {{ intdiv('a', 1); }}\n"
        )),
    );
    assert_eq!(mismatches(&unknown, Engine::pinned()).len(), 1);
}

#[test]
fn the_folded_id_is_a_positive_int_for_a_later_judgment() {
    // `int<1, max>` rather than `int` is the whole of the id fold, so the pin is
    // a position that can tell the two apart.
    let src = in_fn(&format!(
        "{OPEN}$id = get_resource_id($h);\npositive($id);\nnegative($id);\n"
    ));
    let src = format!(
        "{src}/** @param positive-int $i */\nfunction positive(int $i): void {{}}\n\
         /** @param negative-int $i */\nfunction negative(int $i): void {{}}\n"
    );
    let out = phpdoc_mismatches(&src, Engine::pinned());
    assert_eq!(out.len(), 1, "the positive position must be silent and the negative convict: {out:?}");
    assert!(out[0].contains("negative-int"), "{}", out[0]);
}
