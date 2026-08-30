//! ADR-0064 seam (v): the `is_*` type-predicate guard vocabulary and strict
//! literal-haystack `in_array` narrowing through the ADR-0052 arm and value-fact lanes.
//!
//! **Both polarities are pinned for every implemented predicate**: the TRUE
//! branch deletes the arms the predicate *refutes*, the FALSE branch the arms
//! it *proves*, and `Maybe` keeps the arm on both (ADR-0052 §2's "an arm dies
//! only on a definite verdict").
//!
//! Three disciplines beyond the spellings: **recognition** — a namespaced
//! (`Foo\is_string`), namespace-relative (`namespace\is_string`), aliased-import
//! or userland-shadowed twin is a DIFFERENT function and narrows nothing, while
//! fully-qualified `\is_string` IS the global builtin; one helper answers this
//! for every recognizer (issue #153), so [`existence_predicate`],
//! `array_guard_predicate` and the out-parameter seed agree by construction
//! (PHP claims measured on php 8.5.9). **`assert()` inherits for free** —
//! `assert(is_int($x))` narrows through the identical walk with no
//! assert-specific plumbing. **The verdict owns death** — a guard its own
//! binding's fact refutes describes an unreachable branch, so the fact DROPS to
//! nothing there rather than being rewritten into the predicate's base or
//! carried in unchanged (the measured FP class). What changed with issue #432 is
//! only what answers *instead*: where the guard deleted every arm of an
//! all-`Verified` declared lane, that lane is now kept empty rather than removed,
//! so the dump reads `*NEVER*` — the position is unreachable — where it used to
//! fall through to the value lane's `unknown`. The value lane's own behaviour is
//! untouched, which is what
//! `a_refuted_guard_drops_the_fact_because_the_verdict_owns_death` still pins.
//!
//! NB: a call invalidates its argument after the statement (by-ref
//! conservatism), so each fixture dumps a binding once per branch, never before
//! the guard.

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

/// The `(true-branch, false-branch)` dump of `guard` over `decl` — the polarity
/// pin shape every predicate test below uses.
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
    // True refutes every non-string arm; false proves (and kills) exactly the string arm.
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
    // The surviving array arm mints its shape fact through the SAME gated helper
    // S4's presence guards use — no second minting path.
    let (t, f) = polarity(SIX, "is_array($v)");
    assert_eq!(t, "array{a: int} (asserted)");
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
    // `float` is a RUNTIME type here: `is_float(5)` is false, so int arms die on
    // the true branch even though the contract crate's acceptance widens int→float.
    let (t, f) = polarity(SIX, "is_float($v)");
    assert_eq!(t, "float");
    assert_eq!(f, "int|string|bool|null|array{a: int} (asserted)");
}

// ---- Family completion ------------------------------------------------------

#[test]
fn is_null_both_polarities() {
    // Near-duplicate of `=== null`, except TRUE also kills every non-null arm —
    // a polarity `Refine::NotNull` never had.
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
    assert_eq!(f, "array{a: int}|null (asserted)");
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
    assert_eq!(t, "array{a: int} (asserted)");
    assert_eq!(f, "int|float|string|bool|null (asserted)");
}

#[test]
fn is_callable_both_polarities() {
    // No *kind* is outright callable (a string may name a function, an array may
    // be `['C', 'm']`), so true only kills the four kinds that never can be.
    let (t, f) = polarity(SIX, "is_callable($v)");
    assert_eq!(t, "string|array{a: int} (asserted)");
    assert_eq!(f, "int|float|string|bool|null|array{a: int} (asserted)");
}

#[test]
fn is_numeric_both_polarities() {
    // `is_numeric(true)` is FALSE, so the bool arm dies on the true branch; int
    // and float are proven so die on the false one; bare `string` survives both.
    let (t, f) = polarity("int|string|bool", "is_numeric($v)");
    assert_eq!(t, "int|string (asserted)");
    assert_eq!(f, "string|bool (asserted)");
}

#[test]
fn is_numeric_wires_the_modeled_numeric_string_predicate() {
    // ADR-0064 §1: the true branch is the first guard to intersect the modeled
    // `StrPreds::NUMERIC` into a string-based fact; false can't subtract it (no
    // negative predicate vocabulary, ADR-0052 §2), so it stays plain `string`.
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
    // A class may implement `Traversable`, so neither polarity deletes a class
    // arm — the FP-safe `Maybe` the kind table encodes. TRUE: `int` refuted, `C` survives.
    let t = "<?php\nclass C {}\n/** @param C|int $v */\nfunction f($v): void {\n\
             if (is_iterable($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(t), vec!["C (asserted)"]);
    // FALSE: `array` proven, `C` survives.
    let f = "<?php\nclass C {}\n/** @param C|array{a: int} $v */\nfunction f($v): void {\n\
             if (is_iterable($v)) { } else { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(f), vec!["C (asserted)"]);
}

// ---- The value-fact lane ----------------------------------------------------

#[test]
fn a_base_naming_predicate_mints_a_fact_over_an_unfacted_binding() {
    // The common real case: a `mixed`/undeclared binding where the arm lane can
    // subtract nothing, so the minted base fact is the whole payoff.
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
    // stratum; none is a single `Fact`, so a `mixed` binding stays `unknown`
    // (the unguarded baseline) rather than being given a guessed one.
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
    // `is_int($s)` on a proven-string binding describes an unreachable branch: it
    // must neither rewrite the fact to `int` nor carry the refuting fact in — the
    // latter is the measured FP class (a call-site descent binding `$name` to `1`
    // made `if (is_string($name))` around `new Identifier($name)` a "proven
    // TypeError" in nikic/PHP-Parser), so the fact drops to nothing instead.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (is_int($s)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["unknown"]);
}

#[test]
fn the_refutation_drop_holds_on_the_false_branch_too() {
    // Symmetric: `!is_string($s)` on a proven-string binding is the unreachable
    // side, and the VALUE lane still drops rather than carrying `string` into it —
    // which is what the sibling above pins and what the measured FP class needed.
    //
    // The declared ARM lane answers first here, and since issue #432 it answers
    // `*NEVER*`: the guard deleted the only arm a native `string $s` seeds, every
    // arm was `Verified`, so the lane is kept-empty rather than dropped and says
    // outright that no value reaches this branch. Strictly more than the `unknown`
    // this pinned before, and the same spelling an exhausted enum case set has had
    // since issue #429 — the two carriers agreeing is the point of the change.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (!is_string($s)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["*NEVER*"]);
}

#[test]
fn a_verified_lane_exhausted_by_two_predicates_is_kept_empty() {
    // The multi-arm form of the same rule, and ADR-0088 §1's own idiom: a native
    // `string|int` whose two arms both die leaves a lane that is present and
    // empty, not absent. The distinction is the whole point — absence is what an
    // undeclared variable looks like, and reading it as "no value reaches here"
    // would be the opposite claim.
    let src = "<?php\nfunction f(string|int $v): void {\n\
               if (is_string($v)) { return; }\n\
               if (is_int($v)) { return; }\n\
               \\PHPStan\\dumpType($v);\n}\n";
    assert_eq!(dumps(src), vec!["*NEVER*"]);
}

#[test]
fn an_asserted_lane_exhausted_the_same_way_still_drops() {
    // The stratum gate, and the reason `subtract_pred_arms` asks `all_verified`
    // before it retains rather than after. The identical guard sequence over a
    // docblock-only union must NOT mint a kept-empty (Verified) emptiness: the
    // engine enforces nothing on an untyped `$v`, so a value outside `string|int`
    // genuinely arrives and this position is genuinely reachable. Emptying a
    // docblock's claim proves nothing, so the lane drops and the value lane's
    // `unknown` answers instead.
    let src = "<?php\n/** @param string|int $v */\nfunction f($v): void {\n\
               if (is_string($v)) { return; }\n\
               if (is_int($v)) { return; }\n\
               \\PHPStan\\dumpType($v);\n}\n";
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
    // Loose `==` membership is neither type-reflexive nor transitive
    // (`in_array(0, ['a'])` was true before PHP 8; `in_array('1e2', ['100'])` is
    // true today), so no sound identity set exists to mint — decline.
    //
    // Declining to NARROW is not destroying (issue #575). `in_array` writes
    // nothing, so the subject keeps the fact it arrived with; it simply gains
    // none. These two assertions read `unknown` until the pure-question
    // recognizer separated the questions.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (in_array($s, ['a', 'b'])) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["string"]);
    let explicit = "<?php\nfunction f(string $s): void {\n\
                    if (in_array($s, ['a', 'b'], false)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(explicit), vec!["string"]);
}

#[test]
fn in_array_declines_a_non_literal_haystack() {
    // A haystack whose members are unknown mints no identity set, so the needle
    // gains nothing — and loses nothing either (issue #575): the call writes no
    // argument, so the declared `string` survives the guard it could not use.
    // Narrowing the needle to the haystack's ELEMENT type is a separate
    // direction, and the one this fixture will move on when it lands.
    let src = "<?php\n/** @param list<string> $allowed */\nfunction f(string $s, array $allowed): void {\n\
               if (in_array($s, $allowed, true)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["string"]);
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

// ---- Recognition discipline (one helper, see module doc; each leg's witness below) ------

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
fn a_fully_qualified_spelling_is_the_global_builtin() {
    // Witness (php 8.5.9): with an `App\is_string` returning false declared
    // alongside, `\is_string("x")` still answers `true` — `\` reaches global.
    let src = "<?php\nnamespace App;\n/** @param int|string $v */\nfunction f($v): void {\n\
               if (\\is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(src), vec!["string".to_owned()]);
}

#[test]
fn a_fully_qualified_spelling_survives_a_same_namespace_homonym() {
    // Witness (php 8.5.9): a fully-qualified name reaches past `App\is_string` to
    // the global one.
    let src = "<?php\nnamespace App;\nfunction is_string($v): bool { return false; }\n\
               /** @param int|string $v */\nfunction f($v): void {\n\
               if (\\is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(src), vec!["string".to_owned()]);
}

#[test]
fn a_namespace_relative_spelling_is_a_different_function() {
    // Witness (php 8.5.9): `namespace\is_string` resolves against the enclosing
    // namespace ONLY, fataling "Call to undefined function App\is_string()" — the
    // stored raw name strips the `namespace\` prefix, so a textual test would
    // wrongly let it through.
    let src = "<?php\nnamespace App;\n/** @param int|string $v */\nfunction f($v): void {\n\
               if (namespace\\is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_ne!(dumps(src), vec!["string".to_owned()]);
}

#[test]
fn a_namespace_relative_spelling_in_the_root_namespace_is_the_builtin() {
    // Witness: `namespace\is_string("x")` answers `true` with no `namespace`
    // declaration — the enclosing namespace there IS global.
    let src = "<?php\n/** @param int|string $v */\nfunction f($v): void {\n\
               if (namespace\\is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(src), vec!["string".to_owned()]);
}

#[test]
fn an_aliased_import_binds_the_spelling_elsewhere() {
    // Witness: the import sends the unqualified call to `Other\thing` with no
    // fallback (a fatal naming `Other\thing()`) however builtin-like it looks.
    let src = "<?php\nnamespace App;\nuse function Other\\thing as is_string;\n\
               /** @param int|string $v */\nfunction f($v): void {\n\
               if (is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_ne!(dumps(src), vec!["string".to_owned()]);
}

#[test]
fn importing_the_global_name_itself_is_still_the_builtin() {
    // The import leg rejects a *different* target, not an import's mere presence.
    let src = "<?php\nnamespace App;\nuse function is_string;\n\
               /** @param int|string $v */\nfunction f($v): void {\n\
               if (is_string($v)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(src), vec!["string".to_owned()]);
}

#[test]
fn the_in_array_recognizer_carries_the_same_rule() {
    // One helper: `in_array` answers exactly as `is_string` did above.
    let fq = "<?php\nnamespace App;\n/** @param int|string $v */\nfunction f($v): void {\n\
              if (\\in_array($v, [1, 2], true)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_eq!(dumps(fq), vec!["1|2".to_owned()]);
    let rel = "<?php\nnamespace App;\n/** @param int|string $v */\nfunction f($v): void {\n\
               if (namespace\\in_array($v, [1, 2], true)) { \\PHPStan\\dumpType($v); }\n}\n";
    assert_ne!(dumps(rel), vec!["1|2".to_owned()]);
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
    // Negated narrows the ARM lane only — "not a string" ≠ "int", so no fact is
    // minted and the surviving declared arm (Asserted) renders instead.
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

/// The dedicated measurement the decline was waiting on (issue #575).
///
/// The old pin declined the family for two named reasons. Both were checked
/// rather than argued away.
///
/// **Locale sensitivity is real but does not reach these claims.** Across `C`,
/// `en_US.UTF-8`, `de_DE.ISO-8859-1` and `tr_TR.UTF-8`: `ctype_digit` does not
/// move at all (the Latin-1 superscript-two byte is rejected everywhere — POSIX
/// fixes the digit class at 0-9), while `ctype_lower`/`ctype_upper` DO move (the
/// Latin-1 e-acute byte is lowercase under `en_US.UTF-8` and not under `C`).
/// The lowercase claim survives that movement because it is "no ASCII uppercase
/// byte", and a locale can only widen which bytes count as lowercase — it cannot
/// make `A` lowercase, since POSIX requires the two classes disjoint.
///
/// **The integer-argument reinterpretation does not reach them either.** A
/// non-string fact is returned unchanged by the predicate application, so the
/// `-128..=255` byte-value reading has no subject here to be wrong about.
#[test]
fn the_ctype_family_was_measured_rather_than_declined() {
    let src = "<?php\nfunction f(string $s): void {\n\
               if (ctype_digit($s)) { \\PHPStan\\dumpType($s); }\n}\n";
    assert_eq!(dumps(src), vec!["numeric-string".to_owned()]);
}


// ---- substring guards prove a non-empty haystack (issue #575) --------------

/// A haystack that contains a **non-empty** needle has at least that needle's
/// length. The three spellings share one rule.
#[test]
fn a_substring_guard_proves_the_haystack_non_empty() {
    for guard in [
        "str_contains($s, 'x')",
        "str_starts_with($s, 'x')",
        "str_ends_with($s, 'xy')",
    ] {
        let src = format!(
            "<?php\nfunction f(string $s): void {{ if ({guard}) {{ \\PHPStan\\dumpType($s); }} }}\n"
        );
        assert_eq!(dumps(&src), vec!["non-empty-string"], "{guard}");
    }
}

/// The empty needle is why the rule reads the literal instead of trusting the
/// name. Measured on PHP 8.5.9, not assumed: `str_contains("", "")` is **true**,
/// and so are the `str_starts_with` / `str_ends_with` pair — an empty needle is
/// found in the empty string, so such a guard proves nothing at all.
#[test]
fn an_empty_needle_proves_nothing() {
    for guard in ["str_contains($s, '')", "str_starts_with($s, '')", "str_ends_with($s, '')"] {
        let src = format!(
            "<?php\nfunction f(string $s): void {{ if ({guard}) {{ \\PHPStan\\dumpType($s); }} }}\n"
        );
        assert_eq!(dumps(&src), vec!["string"], "{guard}");
    }
}

/// A needle that is not a literal may be `''`, and there is no rung here that
/// proves it is not — so the guard declines rather than guessing.
#[test]
fn a_variable_needle_proves_nothing() {
    let src = "<?php\nfunction f(string $s, string $n): void { if (str_contains($s, $n)) { \\PHPStan\\dumpType($s); } }\n";
    assert_eq!(dumps(src), vec!["string"]);
}

/// Positive-only by construction: what these guards prove is an EXISTENCE, and
/// the failure of an existence proves nothing about the subject. `''` and
/// `'abc'` both fail `str_contains($s, 'x')`.
#[test]
fn the_false_branch_proves_nothing() {
    let src = "<?php\nfunction f(string $s): void { if (!str_contains($s, 'x')) { \\PHPStan\\dumpType($s); } }\n";
    assert_eq!(dumps(src), vec!["string"]);
}

/// The guard adds what it proved and takes nothing away — it is not a
/// subtraction, so a lane that already knew more keeps it.
#[test]
fn an_existing_refinement_survives_the_proof() {
    let src = "<?php\n/** @param numeric-string $s */\nfunction f(string $s): void { if (str_contains($s, '1')) { \\PHPStan\\dumpType($s); } }\n";
    let got = dumps(src);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("numeric-string"), "the guard must not erase what was known: {got:?}");
}

/// The existence questions prove what a true answer implies about the NAME.
/// Measured at 8.5.9: `class_exists('')` and `class_exists('0')` are both false,
/// so a name the engine resolved to a class-like is a `class-string`, which is
/// the predicate covering class, interface, trait and enum together.
#[test]
fn the_class_existence_questions_prove_a_class_string() {
    for guard in [
        "class_exists($c)",
        "interface_exists($c)",
        "enum_exists($c)",
        "trait_exists($c)",
        "class_exists($c, false)",
    ] {
        let src = format!(
            "<?php\nfunction f(string $c): void {{ if ({guard}) {{ \\PHPStan\\dumpType($c); }} }}\n"
        );
        assert_eq!(dumps(&src), vec!["class-string"], "{guard}");
    }
}

/// `function_exists` and `defined` prove only NON-EMPTY, and naming them here
/// rather than omitting them is the point: a function or constant name is not a
/// class-string, so the weaker proof is a decision. Both answer false for `""`
/// (measured), which is what the weaker proof stands on.
#[test]
fn the_other_existence_questions_prove_only_non_empty() {
    for guard in ["function_exists($n)", "defined($n)"] {
        let src = format!(
            "<?php\nfunction f(string $n): void {{ if ({guard}) {{ \\PHPStan\\dumpType($n); }} }}\n"
        );
        assert_eq!(dumps(&src), vec!["non-empty-string"], "{guard}");
    }
}

/// The false branch of an existence question proves nothing — a name that does
/// not resolve may be anything, `''` included.
#[test]
fn a_failed_existence_proves_nothing() {
    let src = "<?php\nfunction f(string $c): void { if (!class_exists($c)) { \\PHPStan\\dumpType($c); } }\n";
    assert_eq!(dumps(src), vec!["string"]);
}

/// The `ctype_*` family, measured at 8.5.9 rather than read off the names. Not
/// one member answers true for `""`, so every one proves non-empty; three prove
/// a character class the string vocabulary can also spell.
#[test]
fn the_ctype_family_proves_non_empty_and_sometimes_more() {
    for (guard, want) in [
        ("ctype_digit($s)", "numeric-string"),
        ("ctype_lower($s)", "non-empty-lowercase-string"),
        ("ctype_upper($s)", "non-empty-uppercase-string"),
        ("ctype_alpha($s)", "non-empty-string"),
        ("ctype_alnum($s)", "non-empty-string"),
        ("ctype_xdigit($s)", "non-empty-string"),
        ("ctype_space($s)", "non-empty-string"),
        ("ctype_punct($s)", "non-empty-string"),
    ] {
        let src = format!(
            "<?php\nfunction f(string $s): void {{ if ({guard}) {{ \\PHPStan\\dumpType($s); }} }}\n"
        );
        assert_eq!(dumps(&src), vec![want], "{guard}");
    }
}

/// The predicate `ctype_digit` does NOT prove, and the reason it would be a lie:
/// `ctype_digit('0')` is true at 8.5.9 and `'0'` is falsy. The implication runs
/// the other way round (`NonFalsy ⇒ NonEmpty`), so claiming the stronger one
/// here would be a false claim about a real string.
#[test]
fn a_ctype_proof_is_non_empty_and_never_non_falsy() {
    let src = "<?php\nfunction f(string $s): void { if (ctype_digit($s)) { \\PHPStan\\dumpType($s); } }\n";
    let got = dumps(src);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(!got[0].contains("non-falsy"), "'0' passes ctype_digit and is falsy: {got:?}");
}

/// The false branch proves nothing: a string failing `ctype_digit` may be
/// anything, `''` included.
#[test]
fn a_failed_ctype_proves_nothing() {
    let src = "<?php\nfunction f(string $s): void { if (!ctype_digit($s)) { \\PHPStan\\dumpType($s); } }\n";
    assert_eq!(dumps(src), vec!["string"]);
}

// ---- the haystack's element subtraction (issue #565) ------------------------

/// The issue's own repro: a strict membership test that FAILS proves the
/// haystack holds no value identical to the needle, which takes the needle out
/// of its ELEMENT type. Nothing about the haystack itself moves — it is still an
/// array — so this is the one subtrahend that reaches inside an arm.
#[test]
fn a_failed_membership_test_subtracts_from_the_element_type() {
    let decl = "/** @param list<?string> $xs */\n";
    for body in [
        "\\assert(!in_array(null, $xs, true)); \\PHPStan\\dumpType($xs);",
        "if (!in_array(null, $xs, true)) { \\PHPStan\\dumpType($xs); }",
    ] {
        let src = format!("<?php\n{decl}function f(array $xs): void {{ {body} }}\n");
        assert_eq!(dumps(&src), vec!["list<string> (asserted)"], "{body}");
    }
}

/// ADR-0052 §2's law, unchanged and applied one level in: an element arm dies
/// only where the subtrahend COVERS it. The literal `1` does not cover `int`, so
/// a `list<int|string>` keeps both arms — the same reason `!== 1` leaves an `int`
/// lane whole at the top level.
#[test]
fn an_element_arm_the_literal_does_not_cover_survives() {
    let src = "<?php\n/** @param list<int|string> $xs */\n\
               function f(array $xs): void { if (!in_array(1, $xs, true)) { \\PHPStan\\dumpType($xs); } }\n";
    assert_eq!(dumps(src), vec!["list<int|string> (asserted)"]);
}

/// Negative-only. The true branch proves at least one element IS the needle,
/// which the element type already admitted — so it subtracts nothing.
#[test]
fn the_membership_branch_subtracts_nothing() {
    let src = "<?php\n/** @param list<?string> $xs */\n\
               function f(array $xs): void { if (in_array(null, $xs, true)) { \\PHPStan\\dumpType($xs); } }\n";
    assert_eq!(dumps(src), vec!["list<string|null> (asserted)"]);
}

/// The two declines, each for the reason `in_array_literals` already records on
/// the other direction: a variable needle names no value the analyzer can
/// subtract, and loose `==` membership mints no sound identity.
#[test]
fn a_variable_needle_and_the_loose_form_subtract_nothing() {
    let var_needle = "<?php\n/** @param list<?string> $xs */\n\
        function f(array $xs, ?string $n): void { if (!in_array($n, $xs, true)) { \\PHPStan\\dumpType($xs); } }\n";
    assert_eq!(dumps(var_needle), vec!["list<string|null> (asserted)"]);
    let loose = "<?php\n/** @param list<?string> $xs */\n\
        function f(array $xs): void { if (!in_array(null, $xs)) { \\PHPStan\\dumpType($xs); } }\n";
    assert_eq!(dumps(loose), vec!["list<string|null> (asserted)"]);
}

/// A map's VALUES are its elements, and its keys are not touched.
#[test]
fn a_map_subtracts_from_its_values_and_not_its_keys() {
    let src = "<?php\n/** @param array<string, int|null> $m */\n\
               function f(array $m): void { if (!in_array(null, $m, true)) { \\PHPStan\\dumpType($m); } }\n";
    assert_eq!(dumps(src), vec!["array<string, int> (asserted)"]);
}

/// An element list the subtraction would empty leaves the arm alone: a container
/// of `Never` is a claim about the program this guard did not make, and an
/// emptied lane is a no-fact signal in this vocabulary rather than a death one.
#[test]
fn an_element_type_the_subtraction_would_empty_is_left_whole() {
    let src = "<?php\n/** @param list<null> $xs */\n\
               function f(array $xs): void { if (!in_array(null, $xs, true)) { \\PHPStan\\dumpType($xs); } }\n";
    assert_eq!(dumps(src), vec!["list<null> (asserted)"]);
}
