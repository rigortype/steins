//! **Userland assertion helpers as guards** — ADR-0058's tag lane meeting
//! ADR-0062's discharge ladder.
//!
//! `assert(isset($d['a']))` discharges a strict-leg finding because its argument
//! survives lowering as a condition. The same claim spelled through a project's
//! own assertion helper — the only `offset.maybe-missing` class the 2026-07-29
//! corpus sweep found — did not, because the value lowering of `isset(…)` is
//! `Other`: nothing was left to consume by the time the callee's
//! `@phpstan-assert true $c` tag was read.
//!
//! Three disciplines are pinned here:
//!
//! * **Same walk.** The helper's condition argument goes through the guard walk
//!   `assert()` uses, so a form the walk does not model narrows nothing rather
//!   than narrowing wrongly.
//! * **The tag is the contract, at the tag's stratum** (ADR-0058): a helper
//!   carrying only a `@phpstan-assert` tag is **Asserted** — the discharge is real
//!   (A-G9) but no proof-layer id may be premised on it. Verified needs the §3
//!   descent proof, which this file does not exercise.
//! * **No tag, no discharge.** An untagged helper — however obviously it throws —
//!   is silent here; its body proof is I2's job.

use steins_domain::Fact;
use steins_infer::{
    DEBUG_TYPE_ID, Diagnostic, Folder, OFFSET_MAYBE_MISSING_ID, OFFSET_UNDECLARED_ID, check_with,
};
use steins_syntax::{ArgValue, SourceTree};

#[derive(Default)]
struct Mock;

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, _name: &str) -> Option<Fact> {
        None
    }
}

/// The assertion-helper class every fixture is written against: the four tag
/// spellings that mean something, plus an untagged twin that throws just as hard.
const HELPERS: &str = r#"
final class Assert {
    /** @phpstan-assert true $c */
    public static function true(bool $c): void { if ($c !== true) { throw new \RuntimeException('x'); } }
    /** @phpstan-assert !false $c */
    public static function notFalse(bool $c): void { if ($c === false) { throw new \RuntimeException('x'); } }
    /** @phpstan-assert false $c */
    public static function false(bool $c): void { if ($c !== false) { throw new \RuntimeException('x'); } }
    /** just a helper */
    public static function untagged(bool $c): void { if ($c !== true) { throw new \RuntimeException('x'); } }
}
/** @phpstan-assert true $c */
function assert_true(bool $c): void { if ($c !== true) { throw new \RuntimeException('x'); } }
"#;

/// A fixture over `@param <decl> $d` with the helper class in scope.
fn fixture(decl: &str, body: &str) -> String {
    format!("<?php\n{HELPERS}\n/** @param {decl} $d */\nfunction probe(array $d): void {{ {body} }}\n")
}

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock)
}

/// The strict-leg ids a source produces, in emission order.
fn ids(src: &str) -> Vec<&'static str> {
    diagnostics(src)
        .into_iter()
        .filter(|d| d.id == OFFSET_UNDECLARED_ID || d.id == OFFSET_MAYBE_MISSING_ID)
        .map(|d| d.id)
        .collect()
}

/// The single `debug.type` body a one-dump source produces.
fn one_type(src: &str) -> String {
    let ds = diagnostics(src);
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

// ---- The discharge itself ---------------------------------------------------

#[test]
fn a_tagged_helper_over_isset_discharges_the_read() {
    // The corpus pattern, verbatim in structure: a helper asserting `isset` on a
    // key, then a read of that key on the next statement.
    let src = fixture("array{a?: string}", "Assert::true(isset($d['a'])); $x = $d['a'];");
    assert!(ids(&src).is_empty(), "the helper-guarded read must be clean: {:?}", diagnostics(&src));
}

#[test]
fn the_helper_narrows_exactly_as_the_native_assert_does() {
    let helper = one_type(&fixture(
        "array{a?: string, b?: string}",
        "Assert::true(isset($d['a'])); \\PHPStan\\dumpType($d);",
    ));
    let native = one_type(&fixture(
        "array{a?: string, b?: string}",
        "assert(isset($d['a'])); \\PHPStan\\dumpType($d);",
    ));
    assert_eq!(helper, native, "the two forms must be the same guard");
    assert_eq!(helper, "dumped type: array{a: string, b?: string} (asserted)");
}

#[test]
fn a_plain_function_helper_discharges_too() {
    // Callee resolution is the existing machinery's; the function world is not a
    // second implementation.
    let src = fixture("array{a?: string}", "assert_true(isset($d['a'])); $x = $d['a'];");
    assert!(ids(&src).is_empty(), "{:?}", diagnostics(&src));
}

#[test]
fn the_not_false_spelling_is_the_same_claim() {
    // `@phpstan-assert !false $c` and `@phpstan-assert true $c` both say "the
    // guard held" — the asserted subject is a condition, so negation is an XOR.
    let src = fixture("array{a?: string}", "Assert::notFalse(isset($d['a'])); $x = $d['a'];");
    assert!(ids(&src).is_empty(), "{:?}", diagnostics(&src));
}

#[test]
fn the_read_still_fires_where_the_helper_guarded_a_different_key() {
    // The vacuity control: a discharge that does not depend on WHICH key was
    // asserted would silence everything.
    let src = fixture("array{a?: string}", "Assert::true(isset($d['zzz'])); $x = $d['a'];");
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
}

#[test]
fn an_untagged_helper_discharges_nothing() {
    // It throws exactly as hard as the tagged one, and Steins does not look:
    // ADR-0058's descent proof (§3) is what reads a body. Without it the tag is
    // the whole contract, and a missing tag is silence.
    let src = fixture("array{a?: string}", "Assert::untagged(isset($d['a'])); $x = $d['a'];");
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
}

#[test]
fn an_unresolvable_callee_discharges_nothing() {
    let src = fixture("array{a?: string}", "\\Nope\\missing(isset($d['a'])); $x = $d['a'];");
    assert_eq!(ids(&src), [OFFSET_MAYBE_MISSING_ID]);
}

// ---- Everything the guard walk already does, arriving for free --------------

#[test]
fn the_disjunctive_cover_travels_through_the_helper() {
    // issue #51's headline pattern, asserted through a helper: the cover is
    // recorded, the `??` supplies `¬isset($d['a'])`, and the final arm is proven.
    let src = fixture(
        "array{a?: string, b?: string}",
        "Assert::true(isset($d['a']) || isset($d['b'])); $x = $d['a'] ?? $d['b'];",
    );
    assert!(ids(&src).is_empty(), "the cover must discharge the final arm: {:?}", diagnostics(&src));
}

#[test]
fn a_conjunction_discharges_both_keys() {
    let src = fixture(
        "array{a?: string, b?: string}",
        "Assert::true(isset($d['a']) && isset($d['b'])); $x = $d['a']; $y = $d['b'];",
    );
    assert!(ids(&src).is_empty(), "{:?}", diagnostics(&src));
}

#[test]
fn array_key_exists_travels_through_the_helper() {
    let src =
        fixture("array{a?: string}", "Assert::true(array_key_exists('a', $d)); $x = $d['a'];");
    assert!(ids(&src).is_empty(), "{:?}", diagnostics(&src));
}

#[test]
fn empty_travels_through_the_helper() {
    let src = fixture("array{a?: string}", "Assert::true(!empty($d['a'])); $x = $d['a'];");
    assert!(ids(&src).is_empty(), "{:?}", diagnostics(&src));
}

#[test]
fn the_false_polarity_is_read_as_the_negated_guard() {
    // `@phpstan-assert false $c` says the guard did NOT hold, so `!isset($d['a'])`
    // — the key is proven absent, and the read that follows is `offset.undeclared`
    // (the sharper id), not `maybe-missing`.
    let src = fixture("array{a?: string}", "Assert::false(isset($d['a'])); $x = $d['a'];");
    assert_eq!(ids(&src), [OFFSET_UNDECLARED_ID]);
}

// ---- The by-ref exemption ---------------------------------------------------

#[test]
fn the_base_survives_the_calls_conservative_invalidation() {
    // A variable mentioned only INSIDE a condition argument cannot be bound by
    // reference (PHP binds a reference to an lvalue, never to the value of
    // `isset(…)`), so the narrowing may outlive the call statement. Without the
    // exemption the discharge would be erased one statement later — which is
    // exactly what the read below would report.
    let src = fixture("array{a?: string}", "Assert::true(isset($d['a'])); $x = $d['a'];");
    assert!(ids(&src).is_empty(), "{:?}", diagnostics(&src));
}

#[test]
fn a_base_also_handed_over_directly_keeps_the_conservative_forgetting() {
    // `H::m(isset($d['a']), $d)` may bind `$d` by reference through the SECOND
    // argument, so the exemption is withheld and the fact is forgotten.
    let src = "<?php\nfinal class A {\n\
               /** @phpstan-assert true $c */\n\
               public static function t(bool $c, array &$r): void { if (!$c) { throw new \\RuntimeException('x'); } }\n\
               }\n\
               /** @param array{a?: string} $d */\n\
               function probe(array $d): void { A::t(isset($d['a']), $d); $x = $d['a']; }\n";
    assert!(
        ids(src).is_empty() || ids(src) == [OFFSET_MAYBE_MISSING_ID],
        "unexpected ids: {:?}",
        ids(src)
    );
    // The discipline that matters: the base was forgotten, so the narrowing did
    // not survive — a by-ref parameter is the one shape that can rewrite it.
    assert_eq!(
        one_type(
            "<?php\nfinal class A {\n\
             /** @phpstan-assert true $c */\n\
             public static function t(bool $c, array &$r): void { if (!$c) { throw new \\RuntimeException('x'); } }\n\
             }\n\
             /** @param array{a?: string} $d */\n\
             function probe(array $d): void { A::t(isset($d['a']), $d); \\PHPStan\\dumpType($d); }\n"
        ),
        "dumped type: unknown"
    );
}

// ---- Scope: what this file deliberately does not do -------------------------

#[test]
fn an_if_true_tag_in_statement_position_is_not_a_statement_assertion() {
    // `-if-true` is conditional on the RETURN VALUE, so a statement-position call
    // establishes nothing — the `Always` kind is the whole statement lane
    // (ADR-0030 Feature D).
    let src = "<?php\nfinal class B {\n\
               /** @phpstan-assert-if-true true $c */\n\
               public static function t(bool $c): bool { return $c; }\n\
               }\n\
               /** @param array{a?: string} $d */\n\
               function probe(array $d): void { B::t(isset($d['a'])); $x = $d['a']; }\n";
    assert_eq!(ids(src), [OFFSET_MAYBE_MISSING_ID]);
}

#[test]
fn an_if_true_tag_in_guard_position_discharges() {
    // The guard-position half of the same tag family: `if (B::t(isset($d['a'])))`
    // routes the condition through the true-branch walk.
    let src = "<?php\nfinal class B {\n\
               /** @phpstan-assert-if-true true $c */\n\
               public static function t(bool $c): bool { return $c; }\n\
               }\n\
               /** @param array{a?: string} $d */\n\
               function probe(array $d): void { if (B::t(isset($d['a']))) { $x = $d['a']; } }\n";
    assert!(ids(src).is_empty(), "{:?}", diagnostics(src));
}

#[test]
fn a_non_boolean_assert_tag_keeps_the_value_lane() {
    // `@phpstan-assert non-empty-string $s` is not a guard claim; it must still go
    // through the value-fact lane it always did.
    let src = "<?php\nfinal class C {\n\
               /** @phpstan-assert string $s */\n\
               public static function str(mixed $s): void { if (!is_string($s)) { throw new \\RuntimeException('x'); } }\n\
               }\n\
               function probe(mixed $s): void { C::str($s); \\PHPStan\\dumpType($s); }\n";
    assert_eq!(one_type(src), "dumped type: string (asserted)");
}
