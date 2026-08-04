//! ADR-0064 seam (v): the `is_*` type-predicate guard vocabulary and strict
//! literal-haystack `in_array` narrowing through the ADR-0052 arm and value-fact lanes.
//!
//! **Both polarities are pinned for every implemented predicate**, because they
//! answer different questions: the TRUE branch deletes the arms the predicate
//! *refutes*, the FALSE branch the arms it *proves*, and `Maybe` keeps the arm on
//! both (ADR-0052 §2's "an arm dies only on a definite verdict").
//!
//! Three disciplines beyond the spellings:
//!
//! * **Recognition discipline.** A namespaced (`Foo\is_string`) or
//!   userland-shadowed twin is a DIFFERENT function and narrows nothing — the
//!   same rule [`existence_predicate`] and `array_guard_predicate` already carry.
//! * **`assert()` inherits for free.** `assert(is_int($x))` narrows through the
//!   identical walk; no assert-specific plumbing exists, and these tests are what
//!   keeps that true.
//! * **The verdict owns death.** A guard its own binding's fact refutes describes
//!   an unreachable branch: the fact DROPS to nothing there — it is neither
//!   rewritten into the predicate's base (a claim about a path the runtime never
//!   takes) nor carried in unchanged (the measured FP class).
//!
//! NB: a variable handed to a call is invalidated after that statement
//! (pre-existing by-ref conservatism), so each fixture dumps a binding once per
//! branch and never before the guard.

use steins_infer::{DEBUG_TYPE_ID, check};
use steins_syntax::SourceTree;

/// Every `debug.type` message body a source produces, in source order.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The `(true-branch, false-branch)` rendering of `guard` over a binding declared
/// `decl` — the polarity pin shape every predicate test below uses.
fn polarity(decl: &str, guard: &str) -> (String, String) {
    let src = format!(
        "<?php\n/** @param {decl} $v */\nfunction f($v): void {{\n\
         if ({guard}) {{ \\PHPStan\\dumpType($v); }} else {{ \\PHPStan\\dumpType($v); }}\n}}\n"
    );
    let d = dumps(&src);
    assert_eq!(d.len(), 2, "expected one dump per branch, got {d:?}");
    (d[0].clone(), d[1].clone())
}

/// The six-kind union every arm-subtraction pin runs over: one arm per PHP
/// runtime kind the domain can spell (object arms live in `narrowing_n4`).
const SIX: &str = "int|string|bool|float|array{a: int}|null";

// ---- The core five (census-ranked) -----------------------------------------

#[test]
fn is_string_both_polarities() {
    // True: every non-string arm is refuted. False: the string arm is proven, so
    // it — and only it — dies.
    let (t, f) = polarity(SIX, "is_string($v)");
    assert_eq!(t, "string");
    assert_eq!(f, "int|float|bool|null|array{a: int} (asserted)");
}

#[test]
fn is_int_both_polarities() {
    let (t, f) = polarity(SIX, "is_int($v)");
    assert_eq!(t, "int");
    assert_eq!(f, "float|string|bool|null|array{a: int} (asserted)");
}

#[test]
fn is_array_both_polarities() {
    // The surviving single array arm mints its shape fact through the SAME gated
    // helper the S4 presence guards use — no second minting path.
    let (t, f) = polarity(SIX, "is_array($v)");
    assert_eq!(t, "non-empty-array{a: int} (asserted)");
    assert_eq!(f, "int|float|string|bool|null (asserted)");
}

#[test]
fn is_bool_both_polarities() {
    let (t, f) = polarity(SIX, "is_bool($v)");
    assert_eq!(t, "bool");
    assert_eq!(f, "int|float|string|null|array{a: int} (asserted)");
}

#[test]
fn is_float_both_polarities() {
    // `float` is read as a RUNTIME type here: `is_float(5)` is false, so the int
    // arms die on the true branch even though the contract crate's acceptance
    // relation lets an int satisfy a declared `float` (PHPStan's widening rule).
    let (t, f) = polarity(SIX, "is_float($v)");
    assert_eq!(t, "float");
    assert_eq!(f, "int|string|bool|null|array{a: int} (asserted)");
}

// ---- Family completion ------------------------------------------------------

#[test]
fn is_null_both_polarities() {
    // The cheap near-duplicate of `=== null` — except the TRUE branch also kills
    // every non-null arm, which `Refine::NotNull` never had a polarity for.
    let (t, f) = polarity(SIX, "is_null($v)");
    assert_eq!(t, "null");
    assert_eq!(f, "int|float|string|bool|array{a: int} (asserted)");
}

#[test]
fn is_scalar_both_polarities() {
    // PHP's `scalar` is exactly int|float|string|bool: `is_scalar(null)` and
    // `is_scalar([])` are both false.
    let (t, f) = polarity(SIX, "is_scalar($v)");
    assert_eq!(t, "int|float|string|bool (asserted)");
    assert_eq!(f, "non-empty-array{a: int}|null (asserted)");
}

#[test]
fn is_object_both_polarities() {
    // No arm of this union is an object, so the true branch empties the lane —
    // which drops to no-fact, never a death signal (ADR-0052 §2).
    let (t, f) = polarity(SIX, "is_object($v)");
    assert_eq!(t, "unknown");
    assert_eq!(f, "int|float|string|bool|null|array{a: int} (asserted)");
}

#[test]
fn is_iterable_both_polarities() {
    // `is_iterable` is `is_array($x) || $x instanceof Traversable`: array arms are
    // proven, scalar/null arms refuted, and an OBJECT arm stays `Maybe` on both
    // branches (see `is_iterable_keeps_object_arms_on_both_branches`).
    let (t, f) = polarity(SIX, "is_iterable($v)");
    assert_eq!(t, "non-empty-array{a: int} (asserted)");
    assert_eq!(f, "int|float|string|bool|null (asserted)");
}

#[test]
fn is_callable_both_polarities() {
    // No *kind* is outright callable — a string may name a function, an array may
    // be a `['C', 'm']` pair — so the true branch only kills the four kinds that
    // can never be callable, and the false branch kills nothing here.
    let (t, f) = polarity(SIX, "is_callable($v)");
    assert_eq!(t, "string|array{a: int} (asserted)");
    assert_eq!(f, "int|float|string|bool|null|array{a: int} (asserted)");
}

#[test]
fn is_numeric_both_polarities() {
    // `is_numeric(true)` is FALSE — bools are not numeric — so the bool arm dies
    // on the true branch; int and float are proven, so they die on the false one.
    // The bare `string` arm is undecided and survives both.
    let (t, f) = polarity("int|string|bool", "is_numeric($v)");
    assert_eq!(t, "int|string (asserted)");
    assert_eq!(f, "string|bool (asserted)");
}

#[test]
fn is_numeric_wires_the_modeled_numeric_string_predicate() {
    // ADR-0064 §1 names this: the true branch is the first guard to intersect the
    // already-modeled `StrPreds::NUMERIC` into a string-based fact. The false
    // branch cannot subtract it — the abstract layers carry no negative predicate
    // vocabulary (ADR-0052 §2) — so it stays plain `string`.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (is_numeric($s)) { \\PHPStan\\dumpType($s); } else { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["numeric-string", "string"]);
}

#[test]
fn is_numeric_decides_string_arms_by_their_predicate_set() {
    // A `numeric-string` arm is PROVEN numeric (dies on the false branch); a
    // non-numeric literal arm is REFUTED (dies on the true branch).
    let (t, f) = polarity("numeric-string|'abc'", "is_numeric($v)");
    assert_eq!(t, "numeric-string (asserted)");
    assert_eq!(f, "'abc' (asserted)");
}

#[test]
fn is_iterable_keeps_object_arms_on_both_branches() {
    // An arbitrary class may implement `Traversable`, so neither polarity may
    // delete a class arm — the FP-safe `Maybe` the kind table encodes.
    // TRUE branch: the `int` arm is refuted and dies, `C` survives as `Maybe`.
    let t = "<?php\nclass C {}\n/** @param C|int $v */\nfunction f($v): void {\n\
             if (is_iterable($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(t), vec!["C (asserted)"]);
    // FALSE branch: the `array` arm is proven and dies, `C` survives as `Maybe`.
    let f = "<?php\nclass C {}\n/** @param C|array{a: int} $v */\nfunction f($v): void {\n\
             if (is_iterable($v)) { } else { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(f), vec!["C (asserted)"]);
}

// ---- The value-fact lane ----------------------------------------------------

#[test]
fn a_base_naming_predicate_mints_a_fact_over_an_unfacted_binding() {
    // The common real-world subject: a `mixed`/undeclared binding, where the arm
    // lane can subtract nothing and the whole payoff is the minted base fact.
    for (guard, want) in [
        ("is_string($v)", "string"),
        ("is_int($v)", "int"),
        ("is_float($v)", "float"),
        ("is_bool($v)", "bool"),
        ("is_null($v)", "null"),
    ] {
        let src = format!(
            "<?php\n/** @param mixed $v */\nfunction f($v): void {{\n\
             if ({guard}) {{ \\PHPStan\\dumpType($v); }}\n}}\n"
        );
        assert_eq!(dumps(&src), vec![want.to_owned()], "guard {guard}");
    }
}

#[test]
fn a_union_naming_predicate_mints_nothing() {
    // `is_scalar`/`is_numeric` name a union of bases and `is_array` the array
    // stratum; none is a single `Fact`, so an unfacted binding stays unfacted
    // rather than being given a guessed one.
    // The unguarded baseline for an unfacted `mixed` binding is `unknown`; each
    // guard below must leave it exactly there.
    let base = "<?php\n/** @param mixed $v */\nfunction f($v): void { \\PHPStan\\dumpType($v); }\n";
    assert_eq!(dumps(base), vec!["unknown".to_owned()]);
    for guard in ["is_scalar($v)", "is_numeric($v)", "is_array($v)", "is_object($v)", "is_callable($v)"] {
        let src = format!(
            "<?php\n/** @param mixed $v */\nfunction f($v): void {{\n\
             if ({guard}) {{ \\PHPStan\\dumpType($v); }}\n}}\n"
        );
        assert_eq!(dumps(&src), vec!["unknown".to_owned()], "guard {guard}");
    }
}

#[test]
fn a_matching_base_keeps_its_refinement_and_drops_nullability() {
    // `is_string` takes its argument by value, so the preceding `!== ''` refinement
    // remains true inside the branch.
    let src = "<?php\nfunction f(string $s): void {\n\
               if ($s !== '' && is_string($s)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["non-empty-string"]);
}

#[test]
fn a_nullable_base_loses_its_null_on_the_true_branch() {
    let src = "<?php\nfunction f(?string $s): void {\n\
               if (is_string($s)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["string"]);
}

#[test]
fn is_null_false_clears_nullability_like_the_landed_not_null_refinement() {
    let src = "<?php\nfunction f(?string $s): void {\n\
               if (!is_null($s)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["string"]);
}

#[test]
fn finite_facts_narrow_by_exact_member_retention_on_both_polarities() {
    let src = "<?php\nfunction f(bool $c): void {\n\
               if ($c) { $v = 5; } else { $v = 'a'; }\n\
               if (is_int($v)) { \\PHPStan\\dumpType($v); } else { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(src), vec!["5", "'a'"]);
}

#[test]
fn a_refuted_guard_drops_the_fact_because_the_verdict_owns_death() {
    // `is_int($s)` on a proven-string binding describes an unreachable branch. It
    // must neither rewrite the fact into `int` nor carry the refuting fact in; the
    // latter is the
    // measured FP class: a call-site descent binding `$name` to `1` made
    // `new Identifier($name)` inside `if (is_string($name))` a "proven TypeError"
    // in nikic/PHP-Parser. The refuting fact therefore drops to nothing.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (is_int($s)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["unknown"]);
}

#[test]
fn the_refutation_drop_holds_on_the_false_branch_too() {
    // Symmetric: `!is_string($s)` on a proven-string binding is the unreachable
    // side, and drops rather than carrying `string` into it.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (!is_string($s)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["unknown"]);
}

#[test]
fn in_array_drops_a_fact_no_haystack_member_can_satisfy() {
    let src = "<?php\nfunction f(bool $c): void {\n\
               if ($c) { $s = 'a'; } else { $s = 'b'; }\n\
               if (in_array($s, ['z'], true)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["unknown"]);
}

// ---- `in_array` (census #3) -------------------------------------------------

#[test]
fn in_array_strict_literal_haystack_narrows_the_needle() {
    let src = "<?php\nfunction f(string $s): void {\n\
               if (in_array($s, ['a', 'b'], true)) { \\PHPStan\\dumpType($s); }\
               else { \\PHPStan\\dumpType($s); }\n}\n";
    // True: the identity set. False: an abstract fact has no point-complement, so
    // nothing is subtracted (ADR-0052 §2, `General`).
    assert_eq!(dumps(src), vec!["'a'|'b'", "string"]);
}

#[test]
fn in_array_false_branch_subtracts_from_a_finite_fact_only() {
    let src = "<?php\nfunction f(bool $c): void {\n\
               if ($c) { $s = 'a'; } else { $s = 'b'; }\n\
               if (in_array($s, ['a'], true)) { \\PHPStan\\dumpType($s); }\
               else { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["'a'", "'b'"]);
}

#[test]
fn in_array_intersects_with_what_is_already_known() {
    // The haystack is not simply adopted: only the literals the existing fact
    // still admits survive.
    let src = "<?php\nfunction f(bool $c): void {\n\
               if ($c) { $s = 'a'; } else { $s = 'b'; }\n\
               if (in_array($s, ['a', 'z'], true)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["'a'"]);
}

#[test]
fn in_array_declines_the_loose_form() {
    // PHP's loose `==` membership is neither type-reflexive nor transitive
    // (`in_array(0, ['a'])` was true before PHP 8; `in_array('1e2', ['100'])` is
    // true today), so there is no sound identity set to mint. Declining leaves the
    // pre-existing retained-guard-call forgetting in place.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (in_array($s, ['a', 'b'])) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["unknown"]);
    let explicit = "<?php\nfunction f(string $s): void {\n\
                    if (in_array($s, ['a', 'b'], false)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(explicit), vec!["unknown"]);
}

#[test]
fn in_array_declines_a_non_literal_haystack() {
    let src = "<?php\n/** @param list<string> $allowed */\nfunction f(string $s, array $allowed): void {\n\
               if (in_array($s, $allowed, true)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["unknown"]);
}

// ---- `assert()` inherits the vocabulary for free ----------------------------

#[test]
fn assert_inherits_every_type_predicate() {
    for (guard, want) in [
        ("is_int($v)", "int"),
        ("is_string($v)", "string"),
        ("is_numeric($v)", "int|string (asserted)"),
    ] {
        let src = format!(
            "<?php\n/** @param int|string|bool $v */\nfunction f($v): void {{\n\
             assert({guard});\n\\PHPStan\\dumpType($v);\n}}\n"
        );
        assert_eq!(dumps(&src), vec![want.to_owned()], "assert({guard})");
    }
}

#[test]
fn assert_inherits_in_array_narrowing() {
    let src = "<?php\nfunction f(string $s): void {\n\
               assert(in_array($s, ['a', 'b'], true));\n\\PHPStan\\dumpType($s);\n}\n";
    assert_eq!(dumps(src), vec!["'a'|'b'"]);
}

// ---- Recognition discipline -------------------------------------------------

#[test]
fn a_namespaced_twin_is_a_different_function() {
    let src = "<?php\nnamespace App;\n/** @param int|string $v */\nfunction f($v): void {\n\
               if (\\App\\is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_ne!(dumps(src), vec!["string".to_owned()]);
}

#[test]
fn a_userland_shadow_is_a_different_function() {
    let src = "<?php\nnamespace App;\nfunction is_string($v): bool { return true; }\n\
               /** @param int|string $v */\nfunction f($v): void {\n\
               if (is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_ne!(dumps(src), vec!["string".to_owned()]);
}

#[test]
fn a_mis_arity_call_is_not_the_predicate() {
    // `is_string()` with no argument (or two) is not the builtin's shape; it
    // narrows nothing rather than reading argument zero of something else.
    let src = "<?php\n/** @param int|string $v */\nfunction f($v): void {\n\
               if (is_string()) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(src), vec!["int|string (asserted)"]);
}

// ---- Polarity distribution through `&&` / `||` / `!` -----------------------

#[test]
fn the_de_morgan_walk_reaches_nested_guards() {
    // `&&` distributes on the true path, `||` on the false path — the same walk
    // `collect_refine` and `collect_shape_guards` use.
    let and = "<?php\n/** @param int|string $v */\nfunction f($v, bool $c): void {\n\
               if ($c && is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(and), vec!["string"]);
    let or = "<?php\n/** @param int|string $v */\nfunction f($v, bool $c): void {\n\
              if ($c || is_string($v)) { } else { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(or), vec!["int (asserted)"]);
    // The negated form narrows the ARM lane only — `!is_string($v)` proves "not a
    // string", which is not the same claim as "int", so no fact is minted and the
    // surviving declared arm (Asserted) is what renders.
    let not = "<?php\n/** @param int|string $v */\nfunction f($v): void {\n\
               if (!is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(not), vec!["int (asserted)"]);
}

#[test]
fn the_guard_threads_into_the_right_operand() {
    // ADR-0052 §6 env threading: `b` in `a && b` evaluates under
    // `then_refinements(a)`, including the type vocabulary.
    let src = "<?php\n/** @param mixed $v */\nfunction f($v): void {\n\
               if (is_string($v) && $v !== '') { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(src), vec!["non-empty-string"]);
}

// ---- The declined family ----------------------------------------------------

#[test]
fn ctype_functions_are_declined_in_this_slice() {
    // `ctype_digit` and kin look like a `StrPreds::DECIMAL_INT` mapping but are
    // locale-sensitive AND (before PHP 8.1) reinterpreted int arguments in
    // `-128..=255` as byte values, so the mapping is not the one the name
    // suggests. They therefore remain declined pending dedicated measurement.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (ctype_digit($s)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_ne!(dumps(src), vec!["numeric-string".to_owned()]);
}
