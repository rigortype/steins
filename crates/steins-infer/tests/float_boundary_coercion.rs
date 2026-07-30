//! Issue #48 — PHP converts at every **native-typed slot** boundary, and the
//! stored fact must be the converted value, not the assigned one.
//!
//! The exposure this fixes was a demonstrated proof-layer FP (the zero-FP
//! surface): an int written to a `float` property read back as the int `1`,
//! `$x === 1` folded true on a value the runtime holds as `1.0`, and the dead
//! branch's `null` premised a "proven" `call.on-null` on code that runs clean.
//! The regression test below is that probe, verbatim.
//!
//! The four nsrt sites that surfaced the gap (`bug-12393.php:40/56`,
//! `bug-12393b.php:42/58`) are the `$this->float = $i` shape — a typed-property
//! write inside a method — pinned here alongside the sibling boundaries the
//! issue names: promoted constructor properties, literal property defaults, a
//! float-declared parameter receiving an int argument (already correct via
//! `coerce_into_param`, pinned so it stays), a typed parameter's default
//! (value-lane silent by construction, pinned as float-based), and a `float`
//! return narrowing an int return (the summary lane drops the value — pinned
//! sound, recorded imprecise).

use steins_domain::Fact;
use steins_infer::{Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The sound-subset folder with the absence family available (no PHP in a unit
/// test; nothing here folds).
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

/// All `debug.type` dump bodies, in source order, asserting no other finding.
fn dumps(src: &str) -> Vec<String> {
    let ds = diagnostics(src);
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "fixture emitted a non-dump finding: {other:?}");
    ds.iter().filter(|d| d.id == "debug.type").map(|d| d.message.clone()).collect()
}

// ---------------------------------------------------------------------------
// The demonstrated proof-layer FP, as a regression test
// ---------------------------------------------------------------------------

/// The probe that answered the issue's exposure question: this source runs
/// clean on real PHP (the else branch executes — `1.0 === 1` is false), so the
/// zero-FP surface must not claim a proven null call on it.
#[test]
fn the_dead_branch_null_call_fp_is_closed() {
    let src = "<?php\n\
        class C { public float $f; }\n\
        class D { public function m(): string { return \"ok\"; } }\n\
        $c = new C();\n\
        $c->f = 1;\n\
        $x = $c->f;\n\
        if ($x === 1) { $y = null; } else { $y = new D(); }\n\
        $y->m();\n";
    let proof: Vec<Diagnostic> =
        diagnostics(src).into_iter().filter(|d| d.id == "call.on-null").collect();
    assert!(proof.is_empty(), "the #48 FP is back: {proof:?}");
}

// ---------------------------------------------------------------------------
// The four-nsrt-site shape: `$this->float = $i` in a method (bug-12393)
// ---------------------------------------------------------------------------

#[test]
fn a_typed_property_write_stores_the_converted_float() {
    // The bug-12393 shape: an int-typed value crossing into a float property
    // through `$this`, read back inside the same method.
    let dumped = dumps(
        "<?php\n\
         class B {\n\
             public float $float;\n\
             public function set(int $i): void {\n\
                 $this->float = 1;\n\
                 \\PHPStan\\dumpType($this->float);\n\
             }\n\
         }\n",
    );
    assert_eq!(dumped, ["dumped type: 1.0"]);
    // The variable-receiver twin.
    let dumped = dumps(
        "<?php\n\
         class C { public float $f; }\n\
         $c = new C();\n\
         $c->f = 1;\n\
         \\PHPStan\\dumpType($c->f);\n",
    );
    assert_eq!(dumped, ["dumped type: 1.0"]);
}

// ---------------------------------------------------------------------------
// Sibling boundaries (the issue's scope list)
// ---------------------------------------------------------------------------

#[test]
fn a_promoted_float_param_stores_the_converted_argument() {
    let dumped = dumps(
        "<?php\n\
         class P { public function __construct(public float $g) {} }\n\
         $p = new P(2);\n\
         \\PHPStan\\dumpType($p->g);\n",
    );
    assert_eq!(dumped, ["dumped type: 2.0"]);
}

#[test]
fn a_float_property_literal_default_stores_the_converted_default() {
    let dumped = dumps(
        "<?php\n\
         class Q { public float $d = 3; }\n\
         $q = new Q();\n\
         \\PHPStan\\dumpType($q->d);\n",
    );
    assert_eq!(dumped, ["dumped type: 3.0"]);
}

/// Already correct before #48 via `coerce_into_param` (the descent binding
/// converts `7` to `7.0` before it enters the callee env; the dump surface
/// shows the standalone walk's `float` seed — the descent walk is summary-only).
/// Pinned so the parameter boundary can never show the unconverted int.
#[test]
fn a_float_param_receiving_an_int_argument_never_shows_the_int() {
    let dumped = dumps(
        "<?php\n\
         function f(float $x, int $unused): void { \\PHPStan\\dumpType($x); }\n\
         f(7, 1);\n",
    );
    assert_eq!(dumped.len(), 1);
    assert!(
        dumped[0] == "dumped type: float" || dumped[0] == "dumped type: 7.0",
        "a float param must read float-based, got {dumped:?}"
    );
}

/// A `float` return over an int return: the summary lane declines the value
/// (consumption is only-stricter-than-the-envelope), so the call site sees the
/// declared envelope, never the unconverted int — imprecise, recorded, sound.
#[test]
fn a_float_return_over_an_int_return_never_leaks_the_int() {
    let dumped = dumps(
        "<?php\n\
         function r(int $n): float { return 1; }\n\
         \\PHPStan\\dumpType(r(5));\n",
    );
    assert_eq!(dumped.len(), 1);
    assert_ne!(dumped[0], "dumped type: 1", "the int leaked through a float return");
}

// ---------------------------------------------------------------------------
// The conversion table's edges (adversarial set)
// ---------------------------------------------------------------------------

#[test]
fn a_union_with_an_int_member_keeps_the_int() {
    // `float|int` performs no conversion on an int — PHP stores the exact match.
    let dumped = dumps(
        "<?php\n\
         class R { public function __construct(public float|int $k) {} }\n\
         $r = new R(5);\n\
         \\PHPStan\\dumpType($r->k);\n",
    );
    assert_eq!(dumped, ["dumped type: 5"]);
}

#[test]
fn a_float_or_false_slot_converts_int_and_keeps_false() {
    let dumped = dumps(
        "<?php\n\
         class R { public function __construct(public float|false $a, public float|false $b) {} }\n\
         $r = new R(4, false);\n\
         \\PHPStan\\dumpType($r->a);\n\
         \\PHPStan\\dumpType($r->b);\n",
    );
    assert_eq!(dumped, ["dumped type: 4.0", "dumped type: false"]);
}

#[test]
fn a_mode_dependent_conversion_drops_to_unknown() {
    // `"5"` into `float`: coercive mode stores 5.0, strict mode fatals — the
    // stored fact is mode-dependent, so nothing sound can be recorded and the
    // slot goes Unknown rather than keeping the string.
    let dumped = dumps(
        "<?php\n\
         class S { public float $f; }\n\
         $s = new S();\n\
         $s->f = \"5\";\n\
         \\PHPStan\\dumpType($s->f);\n",
    );
    assert_eq!(dumped, ["dumped type: unknown"]);
}

#[test]
fn an_untyped_property_still_stores_the_assigned_value_verbatim() {
    // No native type, no conversion: PHP stores the int as an int.
    let dumped = dumps(
        "<?php\n\
         class U { public $p; }\n\
         $u = new U();\n\
         $u->p = 1;\n\
         \\PHPStan\\dumpType($u->p);\n",
    );
    assert_eq!(dumped, ["dumped type: 1"]);
}

#[test]
fn a_float_write_to_a_float_slot_is_untouched() {
    let dumped = dumps(
        "<?php\n\
         class S { public float $f; }\n\
         $s = new S();\n\
         $s->f = 2.5;\n\
         \\PHPStan\\dumpType($s->f);\n",
    );
    assert_eq!(dumped, ["dumped type: 2.5"]);
}