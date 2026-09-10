//! ADR-0093 §3 / issue #618 — an object is spelled in the CONTRACT lane.
//!
//! The ruling grew the spelling, not the domain: `ContractTy::Class` was already
//! a live arm and the mined floor already shipped `date_create` as
//! `DateTime|false`, but no speller would write a mixed class-and-scalar list, so
//! those rows dumped `unknown`. Two halves are pinned here — the **guard** side
//! (the arm lane subtracts the `false` arm and the survivor is a usable receiver)
//! and the **sourcing rule** of §3.1 (an object arm enters the lane only from a
//! declaration; a computed answer never mints one, and §3.2's array-of-objects
//! rows stay refused, filed as #675).
//!
//! The value domain is untouched, and that is the point: no `Val` and no `Fact`
//! is an object here either. Everything below rides the arm lane alone.

use steins_infer::{
    CALL_UNDEFINED_METHOD_ID, DEBUG_TYPE_ID, Diagnostic, Folder, PHPDOC_UNDEFINED_METHOD_ID, check,
    check_with,
};
use steins_syntax::{ArgValue, SourceTree};

/// A folder that folds nothing and reflects nothing, but whose **boot surface**
/// is live and complete — the one thing the absence families (S6 among them)
/// require before they may speak at all. Without it every member claim below
/// would be silent for a reason that has nothing to do with this slice.
struct BootSurface;

impl Folder for BootSurface {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_function(&mut self, _fqn: &str) -> Option<bool> {
        Some(true)
    }
    fn boot_surface_label(&mut self) -> Option<String> {
        Some("PHP 8.5.8 (32 extensions)".to_owned())
    }
    /// Honest about both classes these tests name: `Box` is the project's own and
    /// has no resident homonym (so S6's A2ii leg does not silence on it), while
    /// `DateTime` is a real builtin class and saying otherwise would manufacture
    /// an absence finding that has nothing to do with this slice.
    fn boot_surface_class_like(&mut self, fqn: &str) -> Option<bool> {
        Some(!fqn.eq_ignore_ascii_case("Box"))
    }
}

/// [`findings`] with that boot surface behind the folder seam.
fn live_findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut BootSurface)
}

/// Every `debug.type` message body under the sound subset (`--no-php`), in
/// source order: the floor answers per NAME there, which is the total case.
fn dumps(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
}

/// The dump inside a body that guards `$d = date_create($s)` with `guard`.
fn after_guard(guard: &str) -> String {
    let src = format!(
        "<?php\nfunction f(string $s): void {{\n\
         $d = date_create($s);\n\
         {guard}\n\
         \\PHPStan\\dumpType($d);\n}}\n"
    );
    dumps(&src).first().cloned().unwrap_or_default()
}

#[test]
fn the_false_arm_of_an_object_row_is_subtracted_by_every_guard_shape() {
    // Unguarded, the whole row (ADR-0093 §3's headline spelling).
    assert_eq!(after_guard(""), "dumped type: DateTime|false (asserted)");

    // The four guard shapes #618 names. None of them is new code: the arm lane
    // has stripped the `false` arm of a `T|false` row since issue #443
    // (`apply_class_narrowing`'s doc, point 3), and #557 added the bare
    // truthiness one. What changed is only that the survivor can now be SPELLED.
    for guard in ["if ($d === false) { return; }", "if (!$d) { return; }", "assert($d !== false);"]
    {
        assert_eq!(after_guard(guard), "dumped type: DateTime (asserted)", "guard: {guard}");
    }
    // `instanceof` subtracts the same way and then binds the `Member` fact at
    // `Verified` (`apply_class_narrowing`'s point 2), which outranks the mined
    // row — so the survivor loses the `(asserted)` marker rather than keeping it.
    assert_eq!(
        after_guard("if ($d instanceof \\DateTime) {} else { return; }"),
        "dumped type: DateTime"
    );

    // The complementary branch keeps the arm the guard selected, and `false`
    // alone still spells `false` — the scalar half of the same list.
    let src = "<?php\nfunction f(string $s): void {\n\
               $d = date_create($s);\n\
               if ($d === false) { \\PHPStan\\dumpType($d); }\n}\n";
    assert_eq!(dumps(src), vec!["dumped type: false".to_owned()]);
}

#[test]
fn the_surviving_object_arm_is_a_receiver_the_declared_lane_can_read() {
    // The end-to-end shape of #618's guard-side requirement: `->format('Y')` on
    // the guarded survivor raises NOTHING. `DateTime` declares `format`, and the
    // point of the pin is that teaching the renderer to spell an object arm did
    // not teach any family to invent a member finding about one.
    let src = "<?php\nfunction f(string $s): void {\n\
               $d = date_create($s);\n\
               if ($d === false) { return; }\n\
               $d->format('Y');\n}\n";
    assert!(findings(src).is_empty(), "{:?}", findings(src));
    assert!(live_findings(src).is_empty(), "{:?}", live_findings(src));

    // S6 itself, on a project class where the lane can actually close: a native
    // `Box|false` return narrowed to one `Verified` class arm is a declared
    // receiver, and a method no arm declares is `call.undefined-method`
    // (ADR-0049 §8 / A13 routes the id by the lane's minimum stratum).
    let src = "<?php\nfinal class Box { public function ok(): void {} }\n\
               function make(string $s): Box|false { return $s === '' ? false : new Box(); }\n\
               function f(string $s): void {\n\
               $b = make($s);\n\
               if ($b === false) { return; }\n\
               \\PHPStan\\dumpType($b);\n\
               $b->nope();\n}\n";
    assert_eq!(dumps(src), vec!["dumped type: Box".to_owned()]);
    let ids: Vec<&str> = live_findings(src).iter().map(|d| d.id).collect();
    assert!(
        ids.contains(&CALL_UNDEFINED_METHOD_ID),
        "the surviving object arm must reach S6: {ids:?}",
    );
    assert!(!ids.contains(&PHPDOC_UNDEFINED_METHOD_ID), "a native return is Verified: {ids:?}");
}

#[test]
fn an_object_arm_never_premises_a_proof_finding() {
    // ADR-0093 §3.1 keeps ADR-0069 §2's grade: `Asserted`, rendered `(asserted)`.
    // The marker is not decoration — it is what the all-Verified premise rule
    // reads, so a mined object row is excluded from every proof-layer finding by
    // construction. Spelling the arm did not change which layer may cite it.
    assert!(after_guard("").ends_with("(asserted)"));
    let src = "<?php\nfunction f(string $s): int {\n\
               $d = date_create($s);\n\
               return strlen($d);\n}\n";
    let proof: Vec<Diagnostic> = findings(src)
        .into_iter()
        .filter(|d| steins_infer::layer(d.id) == Some(steins_infer::Layer::Proof))
        .collect();
    assert!(proof.is_empty(), "an object arm premised a proof finding: {proof:?}");
}

#[test]
fn a_computed_answer_never_mints_an_object_arm() {
    // §3.1's sourcing rule, from the refusal side. An object arm may enter the
    // lane only from a DECLARATION — a mined builtin return, an argument's own
    // contract arms, or the exact class of a heap object an argument denotes.
    // These three are the shapes a transfer rule would have to invent one for,
    // and no third lane was opened for them (§3.1), so each stays `unknown`.
    //
    // The first two are #675's population (§3.2): the argument is an array OF
    // objects, and `Val::Array` holds `Val`, no `Val` being an object — so there
    // is nothing to project even before the ruling is consulted.
    let src = "<?php\nfunction f(\\DateTime $a, \\DateTime $b): void {\n\
               $xs = [$a, $b];\n\
               \\PHPStan\\dumpType(min($xs));\n\
               \\PHPStan\\dumpType(array_pop($xs));\n\
               \\PHPStan\\dumpType(new \\DateTime('now'));\n}\n";
    assert_eq!(
        dumps(src),
        vec![
            "dumped type: unknown".to_owned(),
            "dumped type: unknown".to_owned(),
            "dumped type: unknown".to_owned(),
        ],
    );
}
