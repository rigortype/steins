//! Issue #288 — the **destructure source** as a read context, and the declared
//! **return shape** reaching the caller's value lane.
//!
//! `[$a, $b] = $m;` reads `$m[0]` and `$m[1]`, exactly as `$x = $m[0];` reads
//! `$m[0]`; PHP warns per absent key. The ADR-0049 §7 A7 whitelist had never named
//! that position — an omission, not a recorded deferral — so the identical facts
//! that fire at an assignment-RHS were silent through a destructure.
//!
//! # Behavioral witnesses at PHP 8.5.9 (`php -r`)
//!
//! ```text
//! $m = ['a' => 1]; [$p, $q] = $m;   → Warning: Undefined array key 0
//!                                     Warning: Undefined array key 1
//! $m = ['a' => 1]; [, $q] = $m;     → Warning: Undefined array key 1   (only)
//! $m = []; [&$r] = $m;              → no warning                       (an alias)
//! ```
//!
//! The pattern's **targets** are write positions and stay silent — the
//! ADR-0049/0052 soundness audit note's G7(e), unchanged by this file.

use steins_domain::Fact;
use steins_infer::{
    Diagnostic, Folder, OFFSET_MAYBE_MISSING_ID, OFFSET_UNDECLARED_ID, check_with,
};
use steins_syntax::{ArgValue, SourceTree};

/// The absence-family boot surface (there is no PHP in a unit test), as in
/// `shape_strict_leg.rs`: the strict leg is not gated on it, the proof leg in the
/// same walk is, and these fixtures exercise both.
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

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", &mut Mock)
}

/// The strict-leg findings a source produces, as `(id, message)`, in emission order.
fn strict(src: &str) -> Vec<(&'static str, String)> {
    diagnostics(src)
        .into_iter()
        .filter(|d| d.id == OFFSET_UNDECLARED_ID || d.id == OFFSET_MAYBE_MISSING_ID)
        .map(|d| (d.id, d.message))
        .collect()
}

fn ids(src: &str) -> Vec<&'static str> {
    strict(src).into_iter().map(|(id, _)| id).collect()
}

/// A one-function fixture: `@param <decl> $d`, body `<body>`.
fn fixture(decl: &str, body: &str) -> String {
    format!("<?php\n/** @param {decl} $d */\nfunction f(array $d): void {{ {body} }}\n")
}

/// A string-keyed map has no integer keys at all, so every positional read of one
/// is a declared absence — issue #288's own reproducer.
const MAP: &str = "array<string, int>";

// ---- It fires --------------------------------------------------------------

#[test]
fn a_positional_destructure_reads_every_index_it_binds() {
    assert_eq!(ids(&fixture(MAP, "[$p, $q] = $d;")), [OFFSET_UNDECLARED_ID; 2]);
}

#[test]
fn the_list_spelling_reads_exactly_what_the_bracket_spelling_reads() {
    assert_eq!(ids(&fixture(MAP, "list($p, $q) = $d;")), ids(&fixture(MAP, "[$p, $q] = $d;")));
}

#[test]
fn the_message_is_the_assignment_rhs_message_for_the_same_read() {
    let (_, destructured) =
        strict(&fixture(MAP, "[$p] = $d;")).into_iter().next().expect("one finding");
    let (_, plain) = strict(&fixture(MAP, "$x = $d[0];")).into_iter().next().expect("one finding");
    assert_eq!(destructured, plain);
}

#[test]
fn a_keyed_destructure_reads_its_own_keys() {
    let src = fixture("array{a: int}", "['a' => $x, 'b' => $y] = $d;");
    assert_eq!(ids(&src), [OFFSET_UNDECLARED_ID], "only the undeclared 'b' fires: {:?}", strict(&src));
    let (_, msg) = strict(&src).into_iter().next().expect("one finding");
    assert!(msg.contains("offset 'b'"), "the message names the absent key: {msg}");
}

#[test]
fn a_hole_consumes_its_index_without_reading_it() {
    // `[, $q]` binds index 1 and never touches index 0 — one finding, for key 1.
    let src = fixture(MAP, "[, $q] = $d;");
    assert_eq!(ids(&src), [OFFSET_UNDECLARED_ID]);
    let (_, msg) = strict(&src).into_iter().next().expect("one finding");
    assert!(msg.contains("offset 1"), "the hole's own index is not read: {msg}");
}

#[test]
fn a_nested_pattern_judges_the_outer_key_it_reads() {
    // The outer read `$d[0]` is judged; the inner `$d[0][0]` names an intermediate
    // base no leg resolves, and is silent exactly as a chained `$d[0][0]` read is.
    let src = fixture(MAP, "[[$p]] = $d;");
    assert_eq!(ids(&src), [OFFSET_UNDECLARED_ID]);
    let (_, msg) = strict(&src).into_iter().next().expect("one finding");
    assert!(msg.contains("offset 0"), "the outer key is the one judged: {msg}");
}

// ---- The return lane -------------------------------------------------------

#[test]
fn a_declared_return_shape_reaches_the_callers_read() {
    // The scalar return lane always carried into the caller; the array lane did not,
    // because the declared arms seeded no value-lane shape fact.
    let src = "<?php\n/** @return array<string, int> */\n\
               function g(): array { return ['a' => 1]; }\n\
               function f(): void { $m = g(); $x = $m[0]; }\n";
    assert_eq!(ids(src), [OFFSET_UNDECLARED_ID]);
}

#[test]
fn a_call_is_judged_as_a_destructure_source_in_its_own_right() {
    // `[$a, $b] = g();` never binds the value to a name, so the source IS the call.
    let src = "<?php\n/** @return array<string, int> */\n\
               function g(): array { return ['a' => 1]; }\n\
               function f(): void { [$a, $b] = g(); }\n";
    assert_eq!(ids(src), [OFFSET_UNDECLARED_ID; 2]);
    let (_, msg) = strict(src).into_iter().next().expect("a finding");
    assert!(msg.contains("g() is array<string, int>"), "the message names the source: {msg}");
}

// ---- It stays quiet --------------------------------------------------------

#[test]
fn a_by_reference_target_is_an_alias_not_a_read() {
    // `[&$p] = $d;` autovivifies `$d[0]` with no warning — the whole pattern is
    // refused rather than read as something PHP does not do.
    assert!(strict(&fixture(MAP, "[&$p, &$q] = $d;")).is_empty());
}

#[test]
fn the_targets_themselves_are_write_positions() {
    // G7(e): `[$d['zzz']] = $src;` writes the key, and a write is not this family's
    // accusation. The source `$src` carries no shape, so nothing fires at all.
    let src = fixture("array{a: int}", "[$d['zzz']] = $src;");
    assert!(strict(&src).is_empty(), "a destructure target stays silent: {:?}", strict(&src));
}

#[test]
fn a_declared_key_destructures_cleanly() {
    assert!(strict(&fixture("array{a: int, b: int}", "['a' => $x, 'b' => $y] = $d;")).is_empty());
}

#[test]
fn a_list_source_is_silent_at_its_declared_tail() {
    // `list<int>`'s unsealed tail admits every int key — out of the strict leg's v1
    // scope (A-G10), so a positional destructure of one is clean.
    assert!(strict(&fixture("list<int>", "[$p, $q] = $d;")).is_empty());
}

#[test]
fn a_dynamic_key_refuses_the_whole_pattern() {
    let src = fixture(MAP, "$k = 'a'; [$k => $x, 1 => $y] = $d;");
    assert!(strict(&src).is_empty(), "an unprovable key reads nothing: {:?}", strict(&src));
}
