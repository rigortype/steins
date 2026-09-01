//! Issue #610 — the shape rung's **property-fetch subject**: `count($o->p)` /
//! `array_is_list($o->p)` / the positional projections over a depth-1 property
//! read, bound from the allocation-keyed heap (ADR-0036) exactly as the
//! assignment form `$v = $o->p` binds — issue #328 L1's "the subject binds by
//! what it resolves to, not by how it was spelled", extended to the fourth
//! spelling.
//!
//! What is pinned here beyond the spellings:
//!
//! * **One binding, the whole family.** The subject binding is shared by
//!   ADR-0062 §4's `count`/`array_is_list`, the witnessed positional
//!   projections (issue #328) and the §7 key-set projections, so the fetch
//!   spelling reaches all of them at once — a projection test guards against a
//!   future name-gated narrowing of the arm.
//! * **The stratum rides the prop's** (ADR-0061 §3): a shape that entered the
//!   heap off a docblock-claimed binding answers `(asserted)`, never laundered
//!   to `Verified` by the heap hop.
//! * **The refusals are the heap's own.** An unbound receiver, an unknown
//!   prop, a prop swept by an escape, and a non-array prop fact each carry no
//!   usable fact and decline to the envelope floor — no new refusal machinery,
//!   and no answer survives its witness.
//! * **Zero emission** (A-G9's corollary): no fixture here may produce a
//!   non-debug finding — the arm adds facts, not findings.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, Refinement};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The S4 + projection mock sidecar: the reflected envelopes the ADR-0061
/// admission gate consults (`count`/`sizeof`/`array_is_list`), plus the
/// `array` return declarations the §7 projection gate reflects.
#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
    types: HashMap<String, String>,
}

impl Mock {
    fn sidecar() -> Mock {
        let mut facts = HashMap::new();
        let non_negative =
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false);
        facts.insert("count".to_owned(), non_negative.clone());
        facts.insert("sizeof".to_owned(), non_negative);
        facts.insert("array_is_list".to_owned(), Fact::General { base: Base::Bool, nullable: false });
        let mut types = HashMap::new();
        for f in ["array_values", "array_keys"] {
            types.insert(f.to_owned(), "array".to_owned());
        }
        Mock { facts, types }
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
}

/// The single `debug.type` body a one-dump source produces, asserting on the
/// way that the source produced NO other finding (the zero-emission
/// discipline).
fn one_type(src: &str) -> String {
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Mock::sidecar())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect();
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a prop-fetch subject emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A holder with an untracked (untyped, defaultless) property, a constructed
/// receiver, one write, one dump. NB: a call statement sweeps an escaped
/// object's props, so each fixture dumps once (the pre-existing conservatism
/// the shape_reads suite notes for env bindings).
fn dump(write: &str, expr: &str) -> String {
    one_type(&format!(
        "<?php\nclass H {{ public $p; }}\nfunction f() {{ $o = new H(); $o->p = {write}; \\PHPStan\\dumpType({expr}); }}\n"
    ))
}

// count / sizeof — the ADR-0062 §4 cardinality row.

#[test]
fn count_of_a_witnessed_prop_is_the_exact_size() {
    assert_eq!(dump("['a', 'b']", "count($o->p)"), "dumped type: 2");
    assert_eq!(dump("[]", "count($o->p)"), "dumped type: 0");
}

#[test]
fn sizeof_is_the_same_row() {
    assert_eq!(dump("['a', 'b']", "sizeof($o->p)"), "dumped type: 2");
}

/// **The issue's headline** (phpstan-src `bug-11642.php`): a declared
/// `non-empty-list` flowing through a property write answers the interval, at
/// the docblock's own stratum — the heap hop launders nothing (ADR-0061 §3).
#[test]
fn count_of_a_declared_shape_written_through_a_prop_is_the_interval_asserted() {
    assert_eq!(
        one_type(
            "<?php\nclass H { public $p; }\n/** @param non-empty-list<string> $xs */\nfunction f(array $xs) { $o = new H(); $o->p = $xs; \\PHPStan\\dumpType(count($o->p)); }\n"
        ),
        "dumped type: int<1, max> (asserted)"
    );
}

/// A construction-time literal default seeds the prop (ADR-0086 §4's
/// no-constructor case), and the fetch subject reads it — no write statement
/// in the trace at all.
#[test]
fn count_of_a_seeded_default_needs_no_write() {
    assert_eq!(
        one_type(
            "<?php\nclass P { /** @var non-empty-list<string> */ public array $ids = ['one', 'two']; }\nfunction f() { $p = new P(); \\PHPStan\\dumpType(count($p->ids)); }\n"
        ),
        "dumped type: 2"
    );
}

/// The `$this` spelling is the same arm (`var = \"this\"`): an in-method write
/// is a heap fact like any other.
#[test]
fn count_of_an_own_prop_written_in_the_method() {
    assert_eq!(
        one_type(
            "<?php\nclass C { public $p; public function m(): void { $this->p = ['x' => 1]; \\PHPStan\\dumpType(count($this->p)); } }\n"
        ),
        "dumped type: 1"
    );
}

// array_is_list — the denotational flag, both verdicts.

#[test]
fn array_is_list_answers_the_flag_of_a_witnessed_prop() {
    assert_eq!(dump("['a', 'b']", "array_is_list($o->p)"), "dumped type: true");
    assert_eq!(dump("['x' => 1]", "array_is_list($o->p)"), "dumped type: false");
}

// The positional projections share the binding (issue #328): a name-gated
// narrowing of the arm would fail here first.

#[test]
fn projections_execute_over_a_witnessed_prop() {
    assert_eq!(dump("['a' => 1, 'b' => 2]", "array_keys($o->p)"), "dumped type: list{'a', 'b'}");
    assert_eq!(dump("['a' => 1, 'b' => 2]", "array_values($o->p)"), "dumped type: list{1, 2}");
}

// The refusals — each declines to the envelope floor (`int<0, max>` is the
// mock's reflected `count` row), never to a wrong answer.

#[test]
fn an_unbound_receiver_declines() {
    assert_eq!(
        one_type("<?php\nfunction f($o) { \\PHPStan\\dumpType(count($o->p)); }\n"),
        "dumped type: int<0, max>"
    );
}

#[test]
fn an_unknown_prop_declines() {
    assert_eq!(dump("['a', 'b']", "count($o->q)"), "dumped type: int<0, max>");
}

/// An object passed into a call escapes, and the call sweeps its non-readonly
/// props (ADR-0036) — the fetch subject reads the post-sweep heap and declines
/// rather than answer off a value the callee may have rewritten.
#[test]
fn a_swept_prop_declines() {
    assert_eq!(
        one_type(
            "<?php\nclass H { public $p; }\nfunction g($x): void {}\nfunction f() { $o = new H(); $o->p = ['a', 'b']; g($o); \\PHPStan\\dumpType(count($o->p)); }\n"
        ),
        "dumped type: int<0, max>"
    );
}

#[test]
fn a_mode_argument_declines() {
    assert_eq!(
        dump("['a', 'b']", "count($o->p, COUNT_RECURSIVE)"),
        "dumped type: int<0, max>"
    );
}

#[test]
fn a_non_array_prop_fact_declines() {
    assert_eq!(dump("42", "count($o->p)"), "dumped type: int<0, max>");
}
