//! Issue #329 — a transfer-rung `Singleton` is a value, not only a fact.
//!
//! `resolve_literal_under`'s `ArgValue::Call` arm consulted only two sources
//! (`resolve_const_fn`, the allowlist fold), not the transfer rung — so a call
//! whose *fact* was a proven `Singleton` still resolved to no *value*, and
//! everything reading values rather than facts was blind to it. One hop
//! through a binding worked and the inline spelling did not, a distinction PHP
//! does not make.
//!
//! The soundness content is the stratum: the rung's own stratum comes back
//! with the value, so a projection over an `Asserted` subject cannot launder
//! into a `Verified` premise by taking the value road instead of the fact road
//! (`an_asserted_subject_does_not_launder`).

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, ArrayKey, SourceTree};

/// A mock PHP: reflected declarations the transfer rungs gate on, plus a
/// miniature `implode`/`count` for a composed fold to execute.
#[derive(Default)]
struct Mock {
    types: HashMap<String, String>,
    facts: HashMap<String, Fact>,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut types = HashMap::new();
        for f in ["array_values", "array_keys", "array_flip", "array_reverse", "array_slice"] {
            types.insert(f.to_owned(), "array".to_owned());
        }
        types.insert("count".to_owned(), "int".to_owned());
        types.insert("implode".to_owned(), "string".to_owned());
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        facts
            .insert("implode".to_owned(), Fact::General { base: Base::String, nullable: false });
        Mock { types, facts }
    }
}

fn entries(arg: &ArgValue) -> Option<&[(ArrayKey, ArgValue)]> {
    match arg {
        ArgValue::Array(items) => Some(items),
        _ => None,
    }
}

fn text(v: &ArgValue) -> Option<String> {
    match v {
        ArgValue::Str(s) => s.as_str().map(ToOwned::to_owned),
        ArgValue::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        match (name.to_ascii_lowercase().as_str(), args) {
            ("count", [a]) => Some(ArgValue::Int(i64::try_from(entries(a)?.len()).ok()?)),
            ("implode", [sep, a]) => {
                let sep = text(sep)?;
                let parts: Option<Vec<String>> =
                    entries(a)?.iter().map(|(_, v)| text(v)).collect();
                Some(ArgValue::Str(parts?.join(&sep).into()))
            }
            _ => None,
        }
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
    /// `array_slice`'s live signature at `PINNED_PHP` (ADR-0064 Amendment B):
    /// the one family member that reads its siblings positionally.
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        name.eq_ignore_ascii_case("array_slice").then_some((4, 2))
    }
}

fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect();
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

fn dump(body: &str) -> String {
    one_type(&format!("<?php\nfunction f(int $x, string $s): void {{ {body} }}\n"))
}

// The motivating expression

/// Issue #327's opening line, and the reason for the whole chain.
#[test]
fn an_inline_projection_decides_an_identity() {
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_keys(['a' => 1, 'b' => 2]) === ['a', 'b']);"),
        "dumped type: true"
    );
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_keys(['a' => 1, 'b' => 2]) === ['b', 'a']);"),
        "dumped type: false"
    );
    // A function-call argument is value position too.
    assert_eq!(
        dump("$ok = array_keys(['a' => 1, 'b' => 2]) === ['a', 'b']; \\PHPStan\\dumpType($ok);"),
        "dumped type: true"
    );
}

#[test]
fn an_inline_projection_is_a_fold_argument() {
    assert_eq!(
        dump("\\PHPStan\\dumpType(implode(',', array_keys(['a' => 1, 'b' => 2])));"),
        "dumped type: 'a,b'"
    );
    assert_eq!(
        dump("$a = ['a', 'b', 'c']; \\PHPStan\\dumpType(implode(',', array_slice($a, 1)));"),
        "dumped type: 'b,c'"
    );
    assert_eq!(
        dump("\\PHPStan\\dumpType(count(array_keys(['a' => $x, 'b' => $x])));"),
        "dumped type: 2"
    );
}

// The bound spelling is unchanged

#[test]
fn one_hop_through_a_binding_answers_what_it_always_did() {
    for (body, want) in [
        ("$a = ['a','b','c']; $r = array_slice($a, 1); \\PHPStan\\dumpType($r);", "dumped type: list{'b', 'c'}"),
        ("$a = ['a','b','c']; $r = array_slice($a, 1); \\PHPStan\\dumpType($r === ['b', 'c']);", "dumped type: true"),
        ("$a = ['a','b','c']; $r = array_slice($a, 1); \\PHPStan\\dumpType(implode(',', $r));", "dumped type: 'b,c'"),
    ] {
        assert_eq!(dump(body), want, "the bound spelling moved: {body}");
    }
}

// Stratum: the value road may not launder

/// The subject's shape is `Asserted` (docblock claim), so the projection is
/// too — taking the value road must not upgrade it. `(asserted)` marks which
/// stratum answered.
#[test]
fn an_asserted_subject_does_not_launder() {
    let src = "<?php\n/** @param list{int, int} $v */\n\
               function f(array $v): void { \\PHPStan\\dumpType(array_keys($v)); }\n";
    assert_eq!(one_type(src), "dumped type: list{0, 1} (asserted)");
}

// Declines stay declines

/// Only a `Singleton` is a value — a `Shape`, interval, or union leaves the
/// value seam silent, so the comparison keeps its `bool` floor.
#[test]
fn a_rung_answer_that_is_not_a_singleton_resolves_to_no_value() {
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_values(['a' => $x]) === [1]);"),
        "dumped type: bool"
    );
    let declared = "<?php\n/** @param array{a: int, b: int} $v */\n\
                    function f(array $v): void { \\PHPStan\\dumpType(array_keys($v) === ['a', 'b']); }\n";
    assert_eq!(one_type(declared), "dumped type: bool");
}

/// No reflected declaration, no rung, no value — precision goes with the
/// engine; soundness does not.
#[test]
fn a_silent_engine_composes_nothing() {
    struct Silent;
    impl Folder for Silent {
        fn fold(&mut self, _n: &str, _a: &[ArgValue]) -> Option<ArgValue> {
            None
        }
    }
    let src = "<?php\nfunction f(): void { \\PHPStan\\dumpType(array_keys(['a' => 1]) === ['a']); }\n";
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Silent)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds[0].message, "dumped type: bool");
}

/// The acceptance relation must judge one value one way, however it arrived —
/// the latent defect this slice exposed: `resolve_cval`'s call arm wrapped
/// whatever the value seam returned in `CVal::Scalar`, correct only while a
/// call could resolve to a scalar. Once a projection's array became visible
/// there, an array sat in the scalar slot and the relation correctly rejected
/// it. Six false positives, all one call site, all `array_values($headers)`.
#[test]
fn one_value_gets_one_verdict_whatever_produced_it() {
    let src = |call: &str| {
        format!(
            "<?php\n/** @param non-empty-list<string> $h */\nfunction take(array $h): void {{}}\n\
             function f(): void {{ $a = ['x']; {call} }}\n"
        )
    };
    for call in [
        "take(['x']);",                          // written literally
        "$b = array_values($a); take($b);",      // through a binding
        "take(array_values($a));",               // inline — the regression
        "take(array_keys(['x' => 1]));",
        "take(array_slice(['w', 'x'], 1));",
    ] {
        let tree = SourceTree::parse(&src(call));
        let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Mock::sidecar())
            .into_iter()
            .filter(|d| d.id.starts_with("phpdoc."))
            .collect();
        assert!(ds.is_empty(), "a satisfied contract was convicted by `{call}`: {ds:?}");
    }
}

/// The other direction, so the fix above is not just blanket silence: an array
/// that genuinely does not inhabit the declared type is still caught inline.
#[test]
fn a_real_violation_still_fires_through_the_value_seam() {
    let src = "<?php\n/** @param non-empty-list<string> $h */\nfunction take(array $h): void {}\n\
               function f(): void { take(array_keys(['a' => 1, 'b' => 2, 5 => 3])); }\n";
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| d.id == "phpdoc.param-mismatch")
        .collect();
    assert_eq!(ds.len(), 1, "the int key 5 is not a string: {ds:?}");
}

/// The rung reaches its arguments through the same value seam it now feeds, so
/// a projection *of* a projection is the re-entrant case: it must terminate
/// and agree with the two-statement spelling.
#[test]
fn a_nested_projection_terminates_and_agrees_with_its_bound_spelling() {
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_values(array_keys(['a' => 1, 'b' => 2])));"),
        "dumped type: list{'a', 'b'}"
    );
    assert_eq!(
        dump("$k = array_keys(['a' => 1, 'b' => 2]); \\PHPStan\\dumpType(array_values($k));"),
        "dumped type: list{'a', 'b'}"
    );
    assert_eq!(
        dump("\\PHPStan\\dumpType(array_reverse(array_keys(['a' => 1, 'b' => 2])));"),
        "dumped type: list{'b', 'a'}"
    );
}
