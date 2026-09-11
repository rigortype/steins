//! ADR-0057 note (issue #603) — the unrepresentable-but-ENFORCED return hint.
//!
//! `: array`, `: object` and `: iterable` lower to no `NativeType`, and before this
//! slice that emptiness did double duty: it left the A2 native oracle with no arm to
//! drop a boundary `TypeError` against, so `join_value_component` refused the whole
//! value summary before A1's vocabulary was ever consulted, and it left the
//! declared-return arm lane with nothing to seed either.
//!
//! PHP checks the declaration on the callee's side of the boundary, so each keyword
//! IS a Verified upper bound on what a returning call hands back. This file pins the
//! three tops in both lanes — A1's proof where the body has one, the envelope alone
//! where it does not — and pins that A2 still drops the exits outside them.
//!
//! As in `return_summary.rs`: a zero-arg callee does not descend in T0, so every
//! fixture carries an unused leading `int $trigger`.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php")
}

fn one_type(src: &str) -> String {
    let ds: Vec<Diagnostic> =
        findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

// ---------------------------------------------------------------------------
// A1 through the envelope: the body's own proof crosses.
// ---------------------------------------------------------------------------

/// The flagship. `: array` no longer refuses the summary, so the literal's shape is
/// what the caller binds — the value component is A1's, exactly as under `: int`.
#[test]
fn an_array_hint_lets_the_bodys_shape_cross() {
    let src = "<?php\n\
        function f(int $trigger): array {\n\
            return [1, 2];\n\
        }\n\
        $x = f(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "list{1, 2}");
}

/// Where A1 has nothing — an untyped property read proves no fact — the envelope
/// alone is the answer, and it is the arm lane that carries it.
#[test]
fn an_array_hint_with_no_proof_binds_the_envelope() {
    let src = "<?php\n\
        final class Box {\n\
            private $items;\n\
            public function all(int $trigger): array { return $this->items; }\n\
        }\n\
        $b = new Box();\n\
        $x = $b->all(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "array");
}

/// The `: object` twin, and the one whose answer stops short. The value domain holds
/// no object (ADR-0035/0043), so `ContractTy::ObjectAny` in the arm lane is the whole
/// of it — ADR-0093 candidate B, which would give the value domain an object, is
/// deferred. And the DUMP still reads `unknown`, because `spell_arms` has no `object`
/// spelling: ADR-0093 §3 adopted mixed class-and-scalar lists and deliberately left
/// the bare `object` keyword refused, which this slice does not reopen.
///
/// Pinned rather than worked around: the arm is in the lane and a reader who changes
/// the speller should see this expectation change with it.
#[test]
fn an_object_hint_is_carried_but_not_yet_spellable() {
    let src = "<?php\n\
        final class Box {\n\
            /** @var mixed */ private $it;\n\
            public function get(int $trigger): object { return $this->it; }\n\
        }\n\
        $b = new Box();\n\
        $x = $b->get(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "unknown");
}

/// That the `: object` arm is genuinely in the lane, observed where the speller is not
/// in the way: a `@return Foo|false` is refined WITHIN the envelope, and `false` is
/// outside it — PHP would raise a `TypeError` returning `false` from `: object`, so
/// the arm is dropped and the caller reads `Foo` alone. Before #603 the native half
/// was empty and both arms survived.
#[test]
fn an_object_envelope_prunes_an_out_of_envelope_docblock_arm() {
    let src = "<?php\n\
        class Foo {}\n\
        final class Box {\n\
            /** @var mixed */ private $it;\n\
            /** @return Foo|false */\n\
            public function get(int $trigger): object { return $this->it; }\n\
        }\n\
        $b = new Box();\n\
        $x = $b->get(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "Foo (asserted)");
}

/// The `: iterable` twin, spelled as PHP defines the keyword: `array|Traversable`.
#[test]
fn an_iterable_hint_binds_array_or_traversable() {
    let src = "<?php\n\
        final class Box {\n\
            private $rows;\n\
            public function rows(int $trigger): iterable { return $this->rows; }\n\
        }\n\
        $b = new Box();\n\
        $x = $b->rows(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "Traversable|array");
}

// ---------------------------------------------------------------------------
// A2 through the envelope: the exits outside it are boundary TypeErrors.
// ---------------------------------------------------------------------------

/// The drop. One path returns a string under `: array` — PHP raises a `TypeError`
/// there and the value never reaches the caller — so A2 records nothing for it and
/// the surviving array path is the whole summary. Without the oracle this join would
/// be `array{0: 1}|'x'`, which is a value no call can produce.
#[test]
fn a2_drops_a_non_array_exit_under_an_array_hint() {
    let src = "<?php\n\
        function f(int $trigger, bool $b): array {\n\
            if ($b) {\n\
                return [1];\n\
            }\n\
            return 'x';\n\
        }\n\
        $x = f(1, true);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "list{1}");
}

/// The same drop under `: iterable`: a string is neither an array nor a `Traversable`.
#[test]
fn a2_drops_a_string_exit_under_an_iterable_hint() {
    let src = "<?php\n\
        function f(int $trigger, bool $b): iterable {\n\
            if ($b) {\n\
                return [1];\n\
            }\n\
            return 'x';\n\
        }\n\
        $x = f(1, true);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "list{1}");
}

// ---------------------------------------------------------------------------
// What the note does NOT widen.
// ---------------------------------------------------------------------------

/// `: void` keeps refusing: it lowers to no envelope and there is no top to enforce
/// — a `void` function's call expression is `null`, which is ADR-0075's question and
/// not this one.
#[test]
fn a_void_hint_still_says_nothing() {
    let src = "<?php\n\
        function f(int $trigger): void {\n\
            return;\n\
        }\n\
        $x = f(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "unknown");
}

/// Only the BARE keyword. `?array` is a two-member envelope `EnforcedTop` cannot
/// spell, so it stays exactly as unrepresentable as it was before #603.
#[test]
fn a_nullable_array_hint_is_not_an_enforced_top() {
    let src = "<?php\n\
        function f(int $trigger): ?array {\n\
            return [1, 2];\n\
        }\n\
        $x = f(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "unknown");
}

/// A `@return` inside the envelope refines within it at `Asserted`, which is the
/// merge `refine_contract_arms` already performed — the native half is simply no
/// longer empty.
#[test]
fn a_docblock_refines_within_the_array_envelope() {
    let src = "<?php\n\
        final class Box {\n\
            private $items;\n\
            /** @return list<string> */\n\
            public function all(int $trigger): array { return $this->items; }\n\
        }\n\
        $b = new Box();\n\
        $x = $b->all(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "list<string> (asserted)");
}
