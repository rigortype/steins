//! ADR-0062 S7 — the two lanes of the positional projections, at fixture level.
//!
//! * **The order-witnessed lane** (§2): an env-resolved `Singleton(Val::Array)`
//!   argument reaches the sidecar fold exactly like a written literal, so the
//!   order-dependent builtins the allowlist already admits answer over the real,
//!   observed insertion order — `$a = ['x', 'y']; count($a)` is `2`, closing the
//!   gap §1 measured.
//! * **The order-declared lane** (§2/§4): a `Fact::Shape` base takes the sound
//!   widening, and NEVER reads field declaration order — `array_key_first` of
//!   `array{a: int, b: int}` is `'a'|'b'`, the declined import of §7.
//!
//! Zero emission (A-G9's corollary) is asserted on every fixture here, exactly as
//! in `shape_reads.rs`: a shape-derived fact never premises a finding.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, ArrayKey, SourceTree};

/// A mock PHP: it answers the reflected declarations the admission gates consult
/// and *executes* the handful of allowlisted builtins the fold-seam tests use.
/// The fold implementation is the point of those tests — it is what proves the
/// argument arrived as a concrete, order-carrying array rather than as `$a`.
#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
    types: HashMap<String, String>,
    absence: bool,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        facts.insert(
            "array_is_list".to_owned(),
            Fact::General { base: Base::Bool, nullable: false },
        );
        facts.insert("in_array".to_owned(), Fact::General { base: Base::Bool, nullable: false });
        facts
            .insert("implode".to_owned(), Fact::General { base: Base::String, nullable: false });
        // The reflected return-type declarations of the projection family (PHP
        // 8.5.8, `ReflectionFunction::getReturnType()`).
        let mut types = HashMap::new();
        for f in ["array_values", "array_keys", "array_flip", "array_reverse", "array_slice"] {
            types.insert(f.to_owned(), "array".to_owned());
        }
        for f in ["array_key_first", "array_key_last"] {
            types.insert(f.to_owned(), "string|int|null".to_owned());
        }
        types.insert("count".to_owned(), "int".to_owned());
        Mock { facts, types, absence: true }
    }
}

/// The concrete entries of a fold argument, or `None` when the argument did not
/// arrive as an array literal at all.
fn entries(arg: &ArgValue) -> Option<&[(ArrayKey, ArgValue)]> {
    match arg {
        ArgValue::Array(items) => Some(items),
        _ => None,
    }
}

fn scalar_text(v: &ArgValue) -> Option<String> {
    match v {
        ArgValue::Str(s) => Some(s.clone()),
        ArgValue::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

impl Folder for Mock {
    /// A miniature PHP for the three array-taking allowlist entries. Every one of
    /// them reads the argument's **witnessed order**, which is exactly what the
    /// fold seam has to deliver.
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        match (name.to_ascii_lowercase().as_str(), args) {
            ("count", [a]) => Some(ArgValue::Int(i64::try_from(entries(a)?.len()).ok()?)),
            ("implode", [sep, a]) => {
                let sep = scalar_text(sep)?;
                let parts: Option<Vec<String>> =
                    entries(a)?.iter().map(|(_, v)| scalar_text(v)).collect();
                Some(ArgValue::Str(parts?.join(&sep)))
            }
            ("in_array", [needle, a]) => {
                let needle = scalar_text(needle)?;
                Some(ArgValue::Bool(
                    entries(a)?.iter().any(|(_, v)| scalar_text(v).as_deref() == Some(&needle)),
                ))
            }
            _ => None,
        }
    }
    fn absence_family_available(&mut self) -> bool {
        self.absence
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock::sidecar())
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced NO other finding.
fn one_type(src: &str) -> String {
    let ds = diagnostics(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a projection emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A shape-declared fixture: `@param <decl> $v`, one dump of `<expr>`.
fn dump(decl: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\n/** @param {decl} $v */\nfunction f(array $v): void {{ \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

/// A value-lane fixture: statements, then one dump.
fn dump_body(body: &str) -> String {
    one_type(&format!("<?php\nfunction f(): void {{ {body} }}\n"))
}

// ---------------------------------------------------------------------------
// The order-witnessed lane: the fold seam (§1's measured gap)
// ---------------------------------------------------------------------------

#[test]
fn a_bound_array_folds_exactly_like_a_written_literal() {
    // THE gap ADR-0062 §1 measured: both of these are `2`.
    assert_eq!(dump_body("\\PHPStan\\dumpType(count(['x', 'y']));"), "dumped type: 2");
    assert_eq!(dump_body("$a = ['x', 'y']; \\PHPStan\\dumpType(count($a));"), "dumped type: 2");
}

#[test]
fn an_unproven_binding_still_widens_to_the_envelope() {
    // Resolution is opportunistic: an argument that does not resolve is judged
    // exactly as it was before, so the envelope stands.
    let src = "<?php\nfunction f(array $u): void { $a = $u; \\PHPStan\\dumpType(count($a)); }\n";
    assert_eq!(one_type(src), "dumped type: int<0, max>");
}

#[test]
fn a_partly_proven_array_literal_folds_through_its_binding() {
    // The element resolves through the env, so the WHOLE array becomes a fold
    // argument — the same rule, one level down.
    assert_eq!(
        dump_body("$x = 'b'; \\PHPStan\\dumpType(count(['a', $x, 'c']));"),
        "dumped type: 3"
    );
}

#[test]
fn the_witnessed_order_is_what_the_order_dependent_builtins_see() {
    // `implode` is order-dependent and the allowlist already admits it; with the
    // seam closed it now answers over a BOUND array — and it answers in the
    // observed insertion order, not in any canonical one.
    assert_eq!(
        dump_body("$a = ['b', 'a']; \\PHPStan\\dumpType(implode(',', $a));"),
        "dumped type: 'b,a'"
    );
    assert_eq!(
        dump_body("$a = ['x', 'y']; \\PHPStan\\dumpType(in_array('y', $a));"),
        "dumped type: true"
    );
}

#[test]
fn a_folded_binding_keeps_flowing() {
    assert_eq!(
        dump_body("$a = ['x', 'y']; $n = count($a); \\PHPStan\\dumpType($n);"),
        "dumped type: 2"
    );
}

#[test]
fn a_declared_shape_argument_is_not_a_fold_argument() {
    // The fold seam is the VALUE lane only: a declared shape has no witnessed
    // order, so `count` takes the §4 shape transfer (an interval, `(asserted)`),
    // never a fold.
    assert_eq!(
        dump("array{a: int, b?: string}", "count($v)"),
        "dumped type: int<1, 2> (asserted)"
    );
}

// ---------------------------------------------------------------------------
// The order-declared lane: symbolic transfers (§4's row)
// ---------------------------------------------------------------------------

#[test]
fn array_values_of_a_shape_is_a_list_of_the_value_union() {
    // Heterogeneous values: `int|string` is not one fact, so the element bound is
    // the unknown floor — the list-ness and non-emptiness still carry.
    assert_eq!(
        dump("array{a: int, b?: string}", "array_values($v)"),
        "dumped type: non-empty-list<mixed> (asserted)"
    );
    // Homogeneous values: the bound survives.
    assert_eq!(
        dump("array{a: int, b?: int}", "array_values($v)"),
        "dumped type: non-empty-list<int> (asserted)"
    );
    assert_eq!(dump("array<string, int>", "array_values($v)"), "dumped type: list<int> (asserted)");
}

#[test]
fn array_keys_of_a_sealed_shape_enumerates_the_key_set() {
    assert_eq!(
        dump("array{a: int, b?: string}", "array_keys($v)"),
        "dumped type: non-empty-list<'a'|'b'> (asserted)"
    );
    assert_eq!(dump("array<string, int>", "array_keys($v)"), "dumped type: list<string> (asserted)");
    // `array-key` is `int|string` — not one fact, so the element bound widens
    // rather than guessing one half of it.
    assert_eq!(dump("array", "array_keys($v)"), "dumped type: list<mixed> (asserted)");
}

#[test]
fn array_key_first_is_some_key_of_the_set_never_the_declared_first() {
    // **The negative test of this slice** (§2, §7's declined import 1): PHPStan
    // answers `'a'` here and is wrong on `['b' => 2, 'a' => 1]`, which the shape
    // admits just as well.
    assert_eq!(dump("array{a: int, b: int}", "array_key_first($v)"), "dumped type: 'a'|'b' (asserted)");
    assert_eq!(dump("array{a: int, b: int}", "array_key_last($v)"), "dumped type: 'a'|'b' (asserted)");
    // A possibly-empty shape adds `null` — PHP's own answer for `[]`.
    assert_eq!(
        dump("array{a?: int, b?: int}", "array_key_first($v)"),
        "dumped type: 'a'|'b'|null (asserted)"
    );
    assert_eq!(
        dump("array<string, int>", "array_key_first($v)"),
        "dumped type: string|null (asserted)"
    );
}

#[test]
fn array_reverse_and_array_flip_take_their_stated_widenings() {
    // A required string key survives the reversal, so the result is never a list;
    // the entry count is preserved, so it stays non-empty.
    assert_eq!(
        dump("array{a: int, b?: int}", "array_reverse($v)"),
        "dumped type: non-empty-associative-array<int> (asserted)"
    );
    // All-int keys are renumbered — the result IS a list.
    assert_eq!(dump("list<string>", "array_reverse($v)"), "dumped type: list<string> (asserted)");
    // `array_flip` drops non-emptiness (a non-`int|string` value is skipped) and
    // claims `array-key` unless every value is an `int`.
    assert_eq!(
        dump("array{a: int, b?: int}", "array_flip($v)"),
        "dumped type: array<int, 'a'|'b'> (asserted)"
    );
}

#[test]
fn the_declined_projections_say_nothing() {
    // `array_slice` (offset/length govern the key structure) and the value side
    // of `array_search` are v1 declines — honest silence, not a wrong widening.
    // Both now show the rung BELOW instead of `unknown`: ADR-0069's Asserted floor,
    // and the `(asserted)` marker is the difference. `array_slice`'s projection would
    // have described the key structure; it says nothing, and the catalog's bare
    // `array` — which describes no key structure at all — stands in its place. That
    // row is one ADR-0071 admitted; the `array_search` row below is a multi-base
    // union #73 counted and dropped and #79 admitted. Neither moved with anything in
    // this family.
    assert_eq!(
        dump("array{a: int, b?: int}", "array_slice($v, 1)"),
        "dumped type: array (asserted)"
    );
    assert_eq!(
        dump("array{a: int, b?: int}", "array_search(1, $v)"),
        "dumped type: int|string|false (asserted)"
    );
}

#[test]
fn a_second_argument_or_a_nullable_base_declines() {
    // The seam is single-argument by construction, and a nullable base may be
    // `null` (a TypeError, not a projection). What is withheld is the PROJECTION —
    // the key structure this family computes from the argument's own shape — and the
    // floor's argument-blind row stands in its place, marked.
    assert_eq!(dump("array{a: int}", "array_reverse($v, true)"), "dumped type: array (asserted)");
    let src = "<?php\n/** @param array{a: int}|null $v */\n\
               function f(?array $v): void { \\PHPStan\\dumpType(array_values($v)); }\n";
    assert_eq!(one_type(src), "dumped type: list<mixed> (asserted)");
}

#[test]
fn a_project_function_shadowing_the_name_declines() {
    // The same shadow rule the fold and the envelope carry: a user `array_values`
    // is not the builtin.
    let src = "<?php\n/** @param array{a: int} $v */\n\
               function array_values(array $x): array { return $x; }\n\
               function f(array $v): void { \\PHPStan\\dumpType(array_values($v)); }\n";
    let ds = diagnostics(src);
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1);
    assert_eq!(ty[0].message, "dumped type: unknown");
}

#[test]
fn without_the_reflected_declaration_the_rule_is_withheld() {
    // The ADR-0061 §2 admission gate: no live PHP (or a monkey-patch extension
    // loaded) means the engine's own declaration is unavailable, and the transfer
    // is withheld rather than trusted. `--no-php` is exactly where ADR-0069's floor
    // is loudest, so the gate's observable is the marker rather than `unknown`: the
    // catalog's `list<mixed>` says the result is a list and nothing more, while the
    // withheld transfer would have carried `$v`'s own element type across.
    struct NoPhp;
    impl Folder for NoPhp {
        fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
            None
        }
    }
    let src = "<?php\n/** @param array{a: int} $v */\n\
               function f(array $v): void { \\PHPStan\\dumpType(array_values($v)); }\n";
    let tree = SourceTree::parse(src);
    let ds = check_with(&tree, &[], "t.php", &mut NoPhp);
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1);
    assert_eq!(ty[0].message, "dumped type: list<mixed> (asserted)");
}
