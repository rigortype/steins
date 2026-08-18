//! Issue #429 — the **enum case domain**: an enum-typed declaration carries the
//! finite, `Verified` set of its declared cases, and `===` / `!==` against a case
//! narrows and subtracts from it (the ADR-0052 §2 arm machinery, with enum cases
//! as the arms).
//!
//! This is the one place in PHP where the runtime-enforced type is finite, so it
//! is the one place an exhaustiveness question is answerable on Verified premises
//! alone. What is pinned here:
//!
//! * **Both directions.** `=== Suit::Hearts` leaves that case on the true arm and
//!   removes it from the false one; `!== Suit::Hearts` is the same guard read the
//!   other way. An `elseif` chain accumulates because each link narrows the else
//!   env the next one starts from — nothing chain-specific is implemented.
//! * **Empty means empty.** A chain covering every case leaves a domain
//!   subtracted to nothing, and it reads as nothing (`*NEVER*`); a chain missing
//!   one case leaves exactly that case. These two are the whole point of the
//!   slice — a consumer that asks the exhaustiveness question reads this.
//! * **The absence discipline** (ADR-0049, ADR-0002). Five ways of not knowing
//!   the whole case set, each pinned to produce NO finite domain rather than a
//!   partial one: an unresolvable name, a conditionally declared enum, a
//!   non-enum, an interface over enums, and a docblock that merely says a case
//!   type. In every one of them the guard narrows nothing and no chain empties.
//! * **What is out of scope, pinned as claiming nothing**: backed-enum `->value`
//!   comparisons (issue #429's own exclusion — the backing value is a separate
//!   question).
//! * **`match`/`switch` over enum cases** (issue #433 lifts the class-constant
//!   refusal issues #430/#431 left in place): a by-value `match`/`switch`
//!   structures like any other now, so the two fixtures below pin what that
//!   buys — a matched arm's own narrowing, and the same no-match subtraction
//!   every other by-value construct already carries.
//! * **Zero emission.** This slice adds narrowing vocabulary, not a check: no
//!   fixture here may produce a non-debug finding. The consumer that reports —
//!   `phpdoc.never-param-reachable` — is issue #428's, and nothing here depends
//!   on it.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// Every `debug.type` body in `src`, in source order, asserting on the way that
/// the source produced no other finding at all.
fn types(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let ds: Vec<Diagnostic> = check(&tree, &functions, "test.php");
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "the enum case domain emitted a finding: {other:?}");
    ds.into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The three-case pure enum every chain fixture narrows.
const SUIT: &str = "enum Suit { case Hearts; case Spades; case Clubs; }";

/// A one-parameter function over `Suit`, with `<body>` as its body.
fn suit_fn(body: &str) -> String {
    format!("<?php\n{SUIT}\nfunction f(Suit $s): void {{ {body} }}\n")
}

/// The dumps a `Suit $s` body produces.
fn suit(body: &str) -> Vec<String> {
    types(&suit_fn(body))
}

// ---------------------------------------------------------------------------
// The domain, and both directions of the guard
// ---------------------------------------------------------------------------

#[test]
fn an_untouched_declaration_still_reads_as_the_enum() {
    // The expansion is what makes the domain subtractable; it is not a new
    // spelling. A parameter nobody narrowed says what its author wrote.
    assert_eq!(suit("\\PHPStan\\dumpType($s);"), ["Suit"]);
}

#[test]
fn identity_narrows_the_true_arm_to_one_case() {
    assert_eq!(
        suit("if ($s === Suit::Hearts) { \\PHPStan\\dumpType($s); }"),
        ["Suit::Hearts"]
    );
}

#[test]
fn identity_subtracts_the_case_from_the_false_arm() {
    assert_eq!(
        suit("if ($s === Suit::Hearts) { return; } \\PHPStan\\dumpType($s);"),
        ["Suit::Spades|Suit::Clubs"]
    );
}

#[test]
fn non_identity_is_the_same_guard_read_the_other_way() {
    // `!==` subtracts on its TRUE arm and keeps the case on its false one.
    assert_eq!(
        suit("if ($s !== Suit::Hearts) { \\PHPStan\\dumpType($s); } else { \\PHPStan\\dumpType($s); }"),
        ["Suit::Spades|Suit::Clubs", "Suit::Hearts"]
    );
}

#[test]
fn a_yoda_comparison_reads_the_same_guard() {
    assert_eq!(
        suit("if (Suit::Hearts === $s) { \\PHPStan\\dumpType($s); }"),
        ["Suit::Hearts"]
    );
}

#[test]
fn a_guard_nested_in_a_conjunction_still_reaches_its_lane() {
    // The `&&`/`||` distribution is `collect_refine`'s, not a second one.
    assert_eq!(
        types(&format!(
            "<?php\n{SUIT}\nfunction f(Suit $s, bool $b): void {{ if ($b && $s !== Suit::Hearts) {{ \\PHPStan\\dumpType($s); }} }}\n"
        )),
        ["Suit::Spades|Suit::Clubs"]
    );
}

// ---------------------------------------------------------------------------
// The chain, and the empty domain it produces
// ---------------------------------------------------------------------------

#[test]
fn a_chain_missing_one_case_leaves_exactly_that_case() {
    // The e1 shape: Clubs still reaches the final else, and says so.
    assert_eq!(
        suit(
            "if ($s === Suit::Hearts) { return; }\
             elseif ($s === Suit::Spades) { return; }\
             else { \\PHPStan\\dumpType($s); }"
        ),
        ["Suit::Clubs"]
    );
}

#[test]
fn an_exhaustive_chain_empties_the_domain() {
    // The e2 shape, and the acceptance criterion: a domain subtracted to nothing
    // reads as nothing at the point a consumer asks.
    assert_eq!(
        suit(
            "if ($s === Suit::Hearts) { return; }\
             elseif ($s === Suit::Spades) { return; }\
             elseif ($s === Suit::Clubs) { return; }\
             else { \\PHPStan\\dumpType($s); }"
        ),
        ["*NEVER*"]
    );
}

#[test]
fn a_chain_of_negations_empties_the_domain_too() {
    // Same accumulation from the other polarity: nothing is chain-specific.
    assert_eq!(
        suit(
            "if ($s !== Suit::Hearts && $s !== Suit::Spades && $s !== Suit::Clubs) \
             { \\PHPStan\\dumpType($s); }"
        ),
        ["*NEVER*"]
    );
}

#[test]
fn a_case_of_a_different_enum_empties_the_domain_at_once() {
    // `$s === Level::Low` over a `Suit $s` can never hold, and the true arm says
    // so — the positive branch subtracts every value that is not `Level::Low`,
    // which is every case `Suit` has.
    assert_eq!(
        types(&format!(
            "<?php\n{SUIT}\nenum Level: int {{ case Low = 1; }}\n\
             function f(Suit $s): void {{ if ($s === Level::Low) {{ \\PHPStan\\dumpType($s); }} }}\n"
        )),
        ["*NEVER*"]
    );
}

#[test]
fn a_reassignment_voids_the_narrowing() {
    // The lane is scope-local and dies with the value it described (ADR-0052 §9):
    // a rebound `$s` no longer satisfies what the guards established.
    assert_eq!(suit("if ($s !== Suit::Hearts) { $s = Suit::Hearts; \\PHPStan\\dumpType($s); }"), [
        "unknown"
    ]);
}

// ---------------------------------------------------------------------------
// Backed enums: the case identity narrows, the backing value does not
// ---------------------------------------------------------------------------

const LEVEL: &str = "enum Level: int { case Low = 1; case High = 2; }";

#[test]
fn a_backed_enum_narrows_on_case_identity_like_a_pure_one() {
    assert_eq!(
        types(&format!(
            "<?php\n{LEVEL}\nfunction f(Level $l): void {{\
             if ($l === Level::Low) {{ \\PHPStan\\dumpType($l); }}\
             elseif ($l === Level::High) {{ \\PHPStan\\dumpType($l); }}\
             else {{ \\PHPStan\\dumpType($l); }} }}\n"
        )),
        ["Level::Low", "Level::High", "*NEVER*"]
    );
}

#[test]
fn a_backing_value_comparison_claims_nothing() {
    // OUT OF SCOPE, pinned: narrowing is on case identity, never on the backing
    // value. `$l->value === 1` is a separate question — it must not narrow the
    // case domain, and a chain built out of it must never empty it.
    assert_eq!(
        types(&format!(
            "<?php\n{LEVEL}\nfunction f(Level $l): void {{\
             if ($l->value === 1) {{ \\PHPStan\\dumpType($l); }}\
             elseif ($l->value === 2) {{ \\PHPStan\\dumpType($l); }}\
             else {{ \\PHPStan\\dumpType($l); }} }}\n"
        )),
        ["Level", "Level", "Level"]
    );
}

#[test]
fn a_case_name_comparison_claims_nothing_either() {
    // The `->name` slot is the pure enum's version of the same exclusion.
    assert_eq!(
        types(&format!(
            "<?php\n{SUIT}\nfunction f(Suit $s): void {{\
             if ($s->name === 'Hearts') {{ \\PHPStan\\dumpType($s); }}\
             else {{ \\PHPStan\\dumpType($s); }} }}\n"
        )),
        ["Suit", "Suit"]
    );
}

// ---------------------------------------------------------------------------
// The absence discipline: no complete case set, no finite domain
// ---------------------------------------------------------------------------

#[test]
fn a_conditionally_declared_enum_yields_no_domain() {
    // ADR-0049 A2i: a sibling branch may declare a different case set under the
    // same name. Nothing narrows, and the chain that would be exhaustive over the
    // visible cases does NOT empty — the silence side, deliberately.
    assert_eq!(
        types(
            "<?php\nif (PHP_VERSION_ID > 80000) { enum Cond { case A; case B; } }\n\
             function f(Cond $c): void {\
             if ($c === Cond::A) { \\PHPStan\\dumpType($c); }\
             elseif ($c === Cond::B) { \\PHPStan\\dumpType($c); }\
             else { \\PHPStan\\dumpType($c); } }\n"
        ),
        ["Cond", "Cond", "Cond"]
    );
}

#[test]
fn an_unresolvable_enum_yields_no_domain() {
    // The name resolves to no declaration at all: there is no case set to read,
    // so there is no domain and no exhaustion to claim.
    assert_eq!(
        types(
            "<?php\nfunction f(\\Vendor\\Missing $m): void {\
             if ($m === \\Vendor\\Missing::A) { \\PHPStan\\dumpType($m); }\
             else { \\PHPStan\\dumpType($m); } }\n"
        ),
        // Unresolvable, so there is no declared casing to recover either — the
        // index key is all there is (ADR-0053 §7).
        ["vendor\\missing", "vendor\\missing"]
    );
}

#[test]
fn an_interface_over_enums_yields_no_domain() {
    // `UnitEnum` admits every enum's every case: finite in no useful sense, and
    // not a set this can enumerate.
    assert_eq!(
        types(&format!(
            "<?php\n{SUIT}\nfunction f(UnitEnum $u): void {{\
             if ($u === Suit::Hearts) {{ \\PHPStan\\dumpType($u); }}\
             else {{ \\PHPStan\\dumpType($u); }} }}\n"
        )),
        ["UnitEnum", "UnitEnum"]
    );
}

#[test]
fn a_plain_class_yields_no_domain() {
    // The gate is `is_enum`, not "has constants": a class constant that happens
    // to be compared by identity narrows nothing.
    assert_eq!(
        types(
            "<?php\nfinal class C { public const int A = 1; }\n\
             function f(C $c): void {\
             if ($c === C::A) { \\PHPStan\\dumpType($c); }\
             else { \\PHPStan\\dumpType($c); } }\n"
        ),
        ["C", "C"]
    );
}

#[test]
fn a_docblock_cannot_mint_the_domain() {
    // ADR-0037's trust order, enforced structurally: the case set is read off a
    // native declaration or not at all. `@param Suit` over an untyped parameter
    // is an `Asserted` arm, and an `Asserted` arm does not expand.
    assert_eq!(
        types(&format!(
            "<?php\n{SUIT}\n/** @param Suit $s */\nfunction f($s): void {{\
             if ($s === Suit::Hearts) {{ \\PHPStan\\dumpType($s); }}\
             else {{ \\PHPStan\\dumpType($s); }} }}\n"
        )),
        ["Suit (asserted)", "Suit (asserted)"]
    );
}

#[test]
fn a_nullable_declaration_keeps_its_null_arm_out_of_the_subtraction() {
    // `?Suit` is the case set PLUS null, so subtracting every case leaves the
    // null arm standing — the domain is not empty and must not read as empty.
    assert_eq!(
        types(&format!(
            "<?php\n{SUIT}\nfunction f(?Suit $s): void {{\
             if ($s === Suit::Hearts) {{ return; }}\
             \\PHPStan\\dumpType($s); }}\n"
        )),
        ["Suit::Spades|Suit::Clubs|null"]
    );
}

// ---------------------------------------------------------------------------
// `match` and `switch` over enum cases (issue #433 lifts the refusal)
// ---------------------------------------------------------------------------

#[test]
fn a_statement_position_match_over_enum_cases_narrows_and_subtracts() {
    // The by-value shape now structures over enum-case arm conditions (issue
    // #433 lifts `usable_operand`'s class-constant refusal). The Hearts arm
    // narrows to exactly that case — the arm-lane twin of the guard-side
    // narrowing pinned above — and the default arm carries the same no-match
    // subtraction every other by-value construct already gets, leaving exactly
    // the two cases the one arm did not cover.
    assert_eq!(
        suit(
            "match ($s) { Suit::Hearts => \\PHPStan\\dumpType($s), default => \\PHPStan\\dumpType($s) };"
        ),
        ["Suit::Hearts", "Suit::Spades|Suit::Clubs"]
    );
}

#[test]
fn a_switch_over_enum_cases_subtracts_but_never_narrows_an_arm() {
    // `switch` compares loosely and binds nothing inside a matched case — its
    // truth set is multi-valued, so no single arm is sound — so the Hearts
    // case body dumps the untouched declaration, not the one case. The default
    // arm still carries the no-match subtraction: `switch`'s subtrahend is the
    // exact literal `Suit::Hearts`, sound to subtract even though (per
    // `subtract_no_match_path`'s own doc) a `switch` residue is never read as
    // coverage EVIDENCE by a consumer that asks the exhaustiveness question —
    // the dump surface asks a different question (what narrowed here), not
    // that one.
    assert_eq!(
        suit(
            "switch ($s) { case Suit::Hearts: \\PHPStan\\dumpType($s); break;\
             default: \\PHPStan\\dumpType($s); break; }"
        ),
        ["Suit", "Suit::Spades|Suit::Clubs"]
    );
}

#[test]
fn a_switch_leaves_the_lane_it_did_not_narrow_alone() {
    // The construct is opaque, which forgets what it touches — but a guard that
    // ran BEFORE it still narrowed, and the opaque construct must not be read as
    // having exhausted anything.
    assert_eq!(
        suit(
            "if ($s === Suit::Hearts) { return; }\
             \\PHPStan\\dumpType($s);\
             switch ($s) { case Suit::Spades: break; default: break; }"
        ),
        ["Suit::Spades|Suit::Clubs"]
    );
}

// ---------------------------------------------------------------------------
// The return direction
// ---------------------------------------------------------------------------

#[test]
fn a_declared_enum_return_carries_the_same_domain() {
    // The case set is the same enforced fact on either side of the boundary, so
    // the declared-return floor seeds it exactly as the parameter seeding does.
    //
    // The floor is where this leg lives, and the shape shows it: `pick` gives the
    // caller no return summary, so the declaration is what the assignment reads.
    // A callee whose body DOES summarize hands the caller a heap object instead,
    // and an object holder has no arm lane to narrow — the return direction's
    // remaining half, which needs a carrier this slice does not build.
    assert_eq!(
        types(&format!(
            "<?php\n{SUIT}\nfunction pick(): Suit {{ throw new LogicException(); }}\n\
             function f(): void {{ $s = pick();\
             \\PHPStan\\dumpType($s);\
             if ($s !== Suit::Hearts) {{ \\PHPStan\\dumpType($s); }} }}\n"
        )),
        ["Suit", "Suit::Spades|Suit::Clubs"]
    );
}
