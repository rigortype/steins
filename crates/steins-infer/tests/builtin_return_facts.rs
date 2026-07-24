//! ADR-0056 R1 — builtin return-fact seeding, exercised through the walk with a
//! mock [`Folder`] standing in for the live sidecar (there is no PHP in a unit
//! test). The mock answers `builtin_return_fact` directly, so these tests cover
//! the *seeding* + *precedence* legs — the reflected-envelope fact reaching the
//! value domain at a call site, folding winning over it, the unique-resolution
//! guard, and the no-sidecar sound subset — while the admission-gate legs (subset
//! check, minor pin) live as pure-function tests inside `lib.rs`.
//!
//! The observable is `phpdoc.return-mismatch` (the ADR-0056 FP channel): a proven
//! bool return meeting a declared `@return` decides `Yes`/`No` through the same
//! contract acceptance the seeded fact premises.

use std::collections::HashMap;

use steins_domain::{Base, Fact};
use steins_infer::{Diagnostic, Folder, RETURN_MISMATCH_ID, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A folder that seeds a fixed return fact per builtin name and can optionally
/// fold literal-arg calls — enough to exercise seeding and the fold-beats-fact
/// precedence without a live sidecar.
#[derive(Default)]
struct Mock {
    /// `name` (lowercased) → the reflected/admitted return fact to seed.
    facts: HashMap<String, Fact>,
    /// `name` → a folded literal value (for the precedence test). Only consulted
    /// when every argument is a literal, mirroring the real fold gate.
    folds: HashMap<String, ArgValue>,
}

impl Mock {
    fn with_fact(mut self, name: &str, fact: Fact) -> Self {
        self.facts.insert(name.to_ascii_lowercase(), fact);
        self
    }
    fn with_fold(mut self, name: &str, val: ArgValue) -> Self {
        self.folds.insert(name.to_ascii_lowercase(), val);
        self
    }
}

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        if !args.iter().all(ArgValue::is_literal) {
            return None;
        }
        self.folds.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
}

fn general(base: Base) -> Fact {
    Fact::General { base, nullable: false }
}

fn return_mismatches(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", folder)
        .into_iter()
        .filter(|d| d.id == RETURN_MISMATCH_ID)
        .collect()
}

// ── seeding: the reflected bool envelope enters the value domain ─────────────

#[test]
fn assigned_builtin_bool_flows_to_a_lying_return_docblock() {
    // `$b = is_int($x)` seeds `$b: bool`; returning it under `@return string` is a
    // real contract violation (a TRUE positive — the fact is the runtime's own).
    let src = "<?php
/** @return string */
function f($x) { $b = is_int($x); return $b; }";
    let mut m = Mock::default().with_fact("is_int", general(Base::Bool));
    assert_eq!(return_mismatches(src, &mut m).len(), 1, "bool return vs @return string must fire");
}

#[test]
fn assigned_builtin_bool_satisfies_a_true_return_docblock() {
    // The same seed against a truthful `@return bool` stays silent.
    let src = "<?php
/** @return bool */
function f($x) { $b = is_int($x); return $b; }";
    let mut m = Mock::default().with_fact("is_int", general(Base::Bool));
    assert!(return_mismatches(src, &mut m).is_empty(), "bool return vs @return bool is fine");
}

// ── no sidecar: the sound subset seeds nothing ──────────────────────────────

#[test]
fn no_sidecar_seeds_nothing() {
    // The default `Folder` (NoFold-equivalent) returns no fact — the mock with an
    // EMPTY fact map is that sound subset. No seed ⇒ no return premise ⇒ silence,
    // even against the lying docblock.
    let src = "<?php
/** @return string */
function f($x) { $b = is_int($x); return $b; }";
    let mut m = Mock::default(); // no facts registered
    assert!(return_mismatches(src, &mut m).is_empty(), "without a sidecar fact, nothing fires");
}

// ── precedence: folding is the floor below the return fact ───────────────────

#[test]
fn folding_beats_the_return_fact() {
    // `strlen("abc")` folds to the Singleton int 3; the (deliberately WRONG) seeded
    // envelope `string` must never override the fold. Under `@return int` the folded
    // int matches, so nothing fires — proving the fold won.
    let src = "<?php
/** @return int */
function f() { $b = strlen(\"abc\"); return $b; }";
    let mut m = Mock::default()
        .with_fold("strlen", ArgValue::Int(3))
        .with_fact("strlen", general(Base::String));
    assert!(
        return_mismatches(src, &mut m).is_empty(),
        "the folded Singleton int must win over the seeded envelope"
    );
}

// ── unique resolution: a user function of the same name shadows the builtin ──

#[test]
fn user_function_shadow_blocks_seeding() {
    // A project `function is_int(...)` shadows the builtin, so the builtin envelope
    // must NOT be seeded (conservative, never an FP). The user function's own return
    // is unknown here, so the lying `@return string` stays silent.
    let src = "<?php
function is_int($x) { return $x; }
/** @return string */
function f($x) { $b = is_int($x); return $b; }";
    let mut m = Mock::default().with_fact("is_int", general(Base::Bool));
    assert!(
        return_mismatches(src, &mut m).is_empty(),
        "a user-defined homonym must block builtin-envelope seeding"
    );
}

// ── value position vs guard position ────────────────────────────────────────

#[test]
fn guard_position_is_untouched() {
    // A bare guard `if (is_int($x))` is NOT a returned value — the return-fact
    // seeding never applies to it (guard-position folding is a separate, untouched
    // path). The returned `"s"` is a genuine string, matching `@return string`.
    let src = "<?php
/** @return string */
function f($x) { if (is_int($x)) { return \"s\"; } return \"t\"; }";
    let mut m = Mock::default().with_fact("is_int", general(Base::Bool));
    assert!(
        return_mismatches(src, &mut m).is_empty(),
        "a guard-position builtin call is not a seeded value"
    );
}
