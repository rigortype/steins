//! Issue #76 — the array **read-position** family at the shape-projection rung,
//! and ADR-0064 Amendment B's **arity second leg**.
//!
//! Ten names read a position rather than restructuring the array: `current reset
//! end next prev key array_pop array_shift array_first array_last`. Nine declare
//! a bare `mixed`, so their rules additionally pin the live signature's arity —
//! exercised here from all three sides (right/wrong/absent arity).
//!
//! # Behavioral witnesses at `PINNED_PHP` (8.5.8, `php -r`)
//!
//! Every arm's boundary was measured, not assumed:
//!
//! ```text
//! current([]) === false                    → true
//! current([1, 2]) === 1                    → true
//! $e = []; array_pop($e) === null          → true
//! $e = []; array_shift($e) === null        → true
//! $n = [1]; next($n) === false             → true
//! $p = [1, 2]; prev($p) === false          → true      (a step off the FRONT)
//! $m = [1, 2]; next($m); prev($m) === 1    → true      (…and a step back onto a value)
//! array_first([]) === null                 → true
//! array_last([]) === null                  → true
//! array_first([1, 2]) === 1                → true
//! $a = ['a' => 1]; end($a) === 1           → true
//! $r = []; reset($r) === false             → true
//! $q = []; end($q) === false               → true
//! key([]) === null                         → true
//! $z = [1, 2, 3]; array_pop($z); count($z) === 2  → true   (the mutation)
//! ```
//!
//! Reflection at the same engine: `current/reset/end/next/prev/key/array_pop/
//! array_shift/array_first/array_last` are each `params_total = 1`,
//! `params_required = 1`; `key` declares `string|int|null`, the other nine
//! `mixed`. (`array_first`/`array_last` are PHP 8.5 additions, resident on the
//! pinned engine — asserted in `steins-sidecar`'s `reflect_reports_the_parameter_counts`.)
//!
//! Zero emission (ADR-0062 A-G9's corollary) is asserted on every fixture, exactly
//! as in `shape_projections.rs`: a shape-derived fact never premises a finding.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The ten names of the family, with their reflected declaration at `PINNED_PHP`.
const FAMILY: &[(&str, &str)] = &[
    ("current", "mixed"),
    ("reset", "mixed"),
    ("end", "mixed"),
    ("next", "mixed"),
    ("prev", "mixed"),
    ("key", "string|int|null"),
    ("array_pop", "mixed"),
    ("array_shift", "mixed"),
    ("array_first", "mixed"),
    ("array_last", "mixed"),
];

/// A mock PHP answering the two reflection surfaces the gates consult: the
/// declaration and (issue #76) the parameter counts.
struct Mock {
    types: HashMap<String, String>,
    arity: HashMap<String, (u32, u32)>,
    facts: HashMap<String, Fact>,
}

impl Mock {
    /// The pinned engine: every family member declared as it really is, with its
    /// real `(1, 1)` arity.
    fn sidecar() -> Mock {
        let mut types = HashMap::new();
        let mut arity = HashMap::new();
        for (name, declared) in FAMILY {
            types.insert((*name).to_owned(), (*declared).to_owned());
            arity.insert((*name).to_owned(), (1, 1));
        }
        types.insert("count".to_owned(), "int".to_owned());
        arity.insert("count".to_owned(), (2, 1));
        let mut facts = HashMap::new();
        facts.insert(
            "count".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        Mock { types, arity, facts }
    }

    /// The same engine with the arity surface missing entirely — an older runner,
    /// or a replay table recorded before the field.
    fn without_arity() -> Mock {
        Mock { arity: HashMap::new(), ..Mock::sidecar() }
    }

    /// The same engine reporting a *different* signature for `name` — a stale rule
    /// caught by its second leg.
    fn with_arity(name: &str, total: u32, required: u32) -> Mock {
        let mut m = Mock::sidecar();
        m.arity.insert(name.to_owned(), (total, required));
        m
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        None
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
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        self.arity.get(&name.to_ascii_lowercase()).copied()
    }
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced NO other finding.
fn one_type_with(src: &str, folder: &mut dyn Folder) -> String {
    let tree = SourceTree::parse(src);
    let ds = check_with(&tree, &[], "t.php", folder);
    // `untyped.*` (ADR-0078, issue #200) is excluded alongside the dumps: these
    // fixtures declare a bare `array` on purpose (the shape under test), and a
    // contract-layer id on the missing value type isn't the transfer speaking.
    let other: Vec<&Diagnostic> = ds
        .iter()
        .filter(|d| !d.id.starts_with("debug.") && !d.id.starts_with("untyped."))
        .collect();
    assert!(other.is_empty(), "a read-position transfer emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A shape-declared fixture: `@param <decl> $v`, one dump of `<expr>`.
fn dump_with(decl: &str, expr: &str, folder: &mut dyn Folder) -> String {
    one_type_with(
        &format!(
            "<?php\n/** @param {decl} $v */\nfunction f(array $v): void {{ \\PHPStan\\dumpType({expr}); }}\n"
        ),
        folder,
    )
}

fn dump(decl: &str, expr: &str) -> String {
    dump_with(decl, expr, &mut Mock::sidecar())
}

// The value forms over a provably NON-EMPTY shape: the union alone

#[test]
fn a_non_empty_shape_answers_the_value_union_alone() {
    // `current([1, 2]) === 1` / `end(['a' => 1]) === 1` / `array_first([1, 2]) === 1`:
    // a required field makes the shape non-empty, so no `false`/`null` arm joins.
    for f in ["current", "reset", "end", "array_pop", "array_shift", "array_first", "array_last"] {
        assert_eq!(
            dump("array{a: int, b: int}", &format!("{f}($v)")),
            "dumped type: int (asserted)",
            "{f} of a non-empty shape is its value union"
        );
    }
    // Homogeneous literals stay literal.
    assert_eq!(dump("array{a: 1, b: 2}", "current($v)"), "dumped type: 1|2 (asserted)");
    // `non-empty-array<string, int>` carries non-emptiness without a single field.
    assert_eq!(
        dump("non-empty-array<string, int>", "array_pop($v)"),
        "dumped type: int (asserted)"
    );
}

#[test]
fn next_and_prev_add_false_even_to_a_non_empty_shape() {
    // THE arm this family gets wrong if copied from `current`: `$n = [1];
    // next($n) === false`, `$p = [1, 2]; prev($p) === false` — a step off either end.
    for f in ["next", "prev"] {
        assert_eq!(
            dump("array{a: 1, b: 2}", &format!("{f}($v)")),
            "dumped type: 1|2|false (asserted)",
            "{f} steps past the end of a non-empty array"
        );
    }
    // Where values are abstract, `∪ false` was unspellable (`int|false` is two
    // bases) and the rule declined. `Fact::Union` (issue #339) answers instead,
    // as `int|bool` not `int|false` — `false` widens to its base entering a
    // union arm: sound but coarser, recorded in ADR-0085 §5.
    assert_eq!(dump("array{a: int, b: int}", "next($v)"), "dumped type: int|bool (asserted)");
    assert_eq!(dump("array{a: int, b: int}", "current($v)"), "dumped type: int (asserted)");
}

// The value forms over a POSSIBLY-EMPTY shape: null for one half, false for the other

#[test]
fn a_possibly_empty_shape_adds_null_to_the_pop_and_first_half() {
    // `array_pop($e = []) === null`, `array_first([]) === null` — never `false`.
    for f in ["array_pop", "array_shift", "array_first", "array_last"] {
        assert_eq!(
            dump("array<string, int>", &format!("{f}($v)")),
            "dumped type: int|null (asserted)",
            "{f} of a possibly-empty shape admits null"
        );
    }
}

#[test]
fn a_possibly_empty_shape_adds_false_to_the_pointer_half() {
    // `current([]) === false`, `reset([]) === false`, `end([]) === false`.
    for f in ["current", "reset", "end"] {
        assert_eq!(
            dump("array{a?: 1, b?: 2}", &format!("{f}($v)")),
            "dumped type: 1|2|false (asserted)",
            "{f} of a possibly-empty shape admits false"
        );
    }
    // Same `int|bool` widening as above, for `current`'s pointer form
    // (issue #339, ADR-0085 §5) — a lost refinement, not a wrong one.
    assert_eq!(dump("array<string, int>", "current($v)"), "dumped type: int|bool (asserted)");
}

#[test]
fn a_provably_empty_shape_answers_exactly_the_empty_array_value() {
    // The sharpest form of the two probes: `array{}` admits only `[]`.
    for f in ["current", "reset", "end", "next", "prev"] {
        assert_eq!(
            dump("array{}", &format!("{f}($v)")),
            "dumped type: false (asserted)",
            "{f} of the empty array is false"
        );
    }
    for f in ["array_pop", "array_shift", "array_first", "array_last"] {
        assert_eq!(
            dump("array{}", &format!("{f}($v)")),
            "dumped type: null (asserted)",
            "{f} of the empty array is null"
        );
    }
}

// `key`: the one member with a real declaration pin

#[test]
fn key_reuses_the_array_key_first_widening_and_its_real_pin() {
    // Some key of the set, never the declared-first one (ADR-0062 §2).
    assert_eq!(dump("array{a: int, b: int}", "key($v)"), "dumped type: 'a'|'b' (asserted)");
    // `key([]) === null`: a possibly-empty shape admits null.
    assert_eq!(dump("array{a?: int, b?: int}", "key($v)"), "dumped type: 'a'|'b'|null (asserted)");
    assert_eq!(dump("array<string, int>", "key($v)"), "dumped type: string|null (asserted)");
    // Its pin is the DECLARATION, so it needs no arity leg: an engine with no arity
    // surface at all still admits `key`, and refuses the `mixed`-declared nine.
    let mut old = Mock::without_arity();
    assert_eq!(
        dump_with("array{a: int, b: int}", "key($v)", &mut old),
        "dumped type: 'a'|'b' (asserted)"
    );
    assert_eq!(dump_with("array{a: int, b: int}", "current($v)", &mut old), "dumped type: unknown");
}

// ADR-0064 Amendment B: the arity second leg, from all three sides

#[test]
fn a_mixed_declaration_alone_does_not_admit_a_rule() {
    // (a) The right arity fires — the baseline the other two are measured against.
    assert_eq!(dump("array{a: int}", "current($v)"), "dumped type: int (asserted)");

    // (b) A WRONG total: this engine's `current` isn't the one-parameter function
    // the rule was written against, so the stale rule says nothing — the
    // declaration (`mixed`) is identical in both worlds, which is the whole point.
    for wrong in [(2, 1), (1, 0), (0, 0), (3, 2)] {
        let mut folder = Mock::with_arity("current", wrong.0, wrong.1);
        assert_eq!(
            dump_with("array{a: int}", "current($v)", &mut folder),
            "dumped type: unknown",
            "a {wrong:?} signature must not admit a rule written for (1, 1)"
        );
    }

    // (c) An ABSENT arity withholds like an absent declaration — a runner with
    // no arity replay table degrades to silence, not the un-countersigned rule.
    let mut old = Mock::without_arity();
    for f in ["current", "reset", "end", "next", "prev", "array_pop", "array_shift",
              "array_first", "array_last"] {
        assert_eq!(
            dump_with("array{a: 1, b: 2}", &format!("{f}($v)"), &mut old),
            "dumped type: unknown",
            "{f} withholds without the arity leg"
        );
    }
}

// Mutation: a read-position call never leaves a stale shape behind

#[test]
fn a_mutating_read_position_call_invalidates_the_argument_fact() {
    // `$z = [1, 2, 3]; array_pop($z); count($z) === 2` — the pre-call count must
    // not survive. Six of the ten take argument 0 by reference
    // (`steins_catalog::out_params`), so the walk drops the binding's fact at statement end.
    //
    // Issue #635 split this loop three ways, and the pre-call count survives in
    // none of them.
    //
    // * `next`/`prev` still DROP: they state no written fact, so the
    //   conservative invalidation is all there is.
    // * `array_pop`/`array_shift` now say what they left — `[1, 2, 3]` minus one
    //   entry is two entries, exactly, and the count follows from the fact
    //   rather than from the stale binding.
    // * `reset`/`end` move the internal pointer and nothing else: `$z = [1, 2,
    //   3]; reset($z);` measures `$z === [1, 2, 3]` at PHP 8.5.9, so `3` here is
    //   the answer and not a leak.
    for (f, want) in [
        ("next", "dumped type: int<0, max>"),
        ("prev", "dumped type: int<0, max>"),
        ("array_pop", "dumped type: 2"),
        ("array_shift", "dumped type: 2"),
        ("reset", "dumped type: 3"),
        ("end", "dumped type: 3"),
    ] {
        let src = format!(
            "<?php\nfunction f(): void {{ $z = [1, 2, 3]; {f}($z); \\PHPStan\\dumpType(count($z)); }}\n"
        );
        assert_eq!(one_type_with(&src, &mut Mock::sidecar()), want, "{f}");
    }
    // The same, one layer up: the shape lane must not answer from a moved shape.
    // A DECLARED field order is not an order (ADR-0062 §7), so `array_pop` here
    // cannot say which key left and falls to its `array` floor — the count is
    // back to nothing, carrying the docblock's own stratum.
    let src = "<?php\n/** @param array{a: int, b: int} $v */\n\
               function f(array $v): void { array_pop($v); \\PHPStan\\dumpType(count($v)); }\n";
    assert_eq!(one_type_with(src, &mut Mock::sidecar()), "dumped type: int<0, max> (asserted)");
    // …while the RETURN, computed from the pre-call shape, is still the sharp one.
    assert_eq!(dump("array{a: int, b: int}", "array_pop($v)"), "dumped type: int (asserted)");
}

// The declines

#[test]
fn no_shape_fact_on_the_argument_declines() {
    let src = "<?php\nfunction f(array $v): void { \\PHPStan\\dumpType(current($v)); }\n";
    assert_eq!(one_type_with(src, &mut Mock::sidecar()), "dumped type: unknown");
    // A nullable base may be `null` — a TypeError, not a read.
    let src = "<?php\n/** @param array{a: int}|null $v */\n\
               function f(?array $v): void { \\PHPStan\\dumpType(current($v)); }\n";
    assert_eq!(one_type_with(src, &mut Mock::sidecar()), "dumped type: unknown");
}

#[test]
fn a_call_that_is_not_one_argument_declines() {
    // The seam is single-argument by construction; a second argument is a different
    // function than the one the arity pin describes.
    assert_eq!(dump("array{a: int}", "current($v, 1)"), "dumped type: unknown");
    assert_eq!(dump("array{a: int}", "array_pop($v, 1)"), "dumped type: unknown");
}

#[test]
fn a_project_function_shadowing_the_name_declines() {
    // The shape docblock belongs to `f` (`$v`), not the shadowing declaration
    // (`$x`) — issue #186: naming a param the signature lacks is now `phpdoc.stale-param`.
    let src = "<?php\n\
               function current(array $x): int { return 1; }\n\
               /** @param array{a: int} $v */\n\
               function f(array $v): void { \\PHPStan\\dumpType(current($v)); }\n";
    assert_eq!(one_type_with(src, &mut Mock::sidecar()), "dumped type: unknown");
}

#[test]
fn an_engine_silent_on_the_declaration_declines() {
    // No live PHP (or a monkey-patch extension loaded): the ADR-0061 §2 gate, which
    // this family inherits unchanged on top of its own second leg.
    struct NoPhp;
    impl Folder for NoPhp {
        fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
            None
        }
    }
    let dumped = |f: &str| {
        let src = format!(
            "<?php\n/** @param array{{a: int, b: int}} $v */\n\
             function f(array $v): void {{ \\PHPStan\\dumpType({f}($v)); }}\n"
        );
        one_type_with(&src, &mut NoPhp)
    };
    for f in ["current", "array_pop", "next"] {
        assert_eq!(dumped(f), "dumped type: unknown", "{f} withholds");
    }
    // `key` withholds too — the rung BELOW, ADR-0069's Asserted declared-return
    // floor (functionMap: `int|string|null`), answers instead. The `(asserted)`
    // marker shows this rule would have said `'a'` and didn't; the row was
    // dropped at issue #73 (multi-base union, no envelope) and admitted by #79.
    assert_eq!(dumped("key"), "dumped type: int|string|null (asserted)");
}

#[test]
fn a_moved_declaration_declines_even_with_the_right_arity() {
    // Both legs are required, not either: an engine whose `current` declares
    // something other than `mixed` is not the engine the rule was written against.
    let mut folder = Mock::sidecar();
    folder.types.insert("current".to_owned(), "int|false".to_owned());
    assert_eq!(dump_with("array{a: int}", "current($v)", &mut folder), "dumped type: unknown");
}
