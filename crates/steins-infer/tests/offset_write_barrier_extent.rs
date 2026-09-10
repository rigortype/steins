//! How wide the barrier in front of an offset write opens (issue #641) —
//! ADR-0063 §2.3's target/exposure legs, read by the value lane.
//!
//! `$a[$k] = v`, `$a[] = v` and `unset($a['k'])` used to erase **every** binding
//! in the scope and put back exactly one, the base they wrote through. Issue #636
//! narrowed what that restore says; this suite pins what the clear *takes*.
//!
//! The two sides, on one store:
//!
//! * `$a = 1; $b = ['k' => 1]; $b['k'] = 42;` — no reference anywhere in the
//!   frame, so nothing but `$b` can have seen the write and `$a` survives it;
//! * `$a = 1; $b = ['k' => &$a]; $b['k'] = 42;` — PHP really does make `$a` 42
//!   (`php -r '$a = 1; $b = ["key" => &$a]; $b["key"] = 42; var_dump($a);'`
//!   prints `int(42)` at 8.5.10), so the barrier stays total and `$a` answers
//!   `unknown`. A narrowed barrier that kept `$a` here would answer `1` — a
//!   **wrong** answer where `unknown` is merely a missing one, which is why the
//!   whole aliasing-carrier half of this file lands before the retention half.
//!
//! Every carrier on the ADR-0001 give-up list gets a row, because each one is a
//! separate way for the frame to have tied two names to one cell, and each is a
//! separate arm of [`write_is_frame_private`]'s decline.
//!
//! The carrier rows pin an **outcome**: a named function's poisoned scope binds
//! nothing, so what keeps `['key' => &$a]` and `foreach ($r as &$v)` honest there
//! is that they are recognised as give-up sites in the first place (issue #641's
//! leg 1) — delete that recognition and the four `&` rows below answer the
//! pre-write value, the wrong answer, not a missing one. The exposure leg itself
//! stays unpinned — a poisoned closure scope binds a declared return in a store
//! lane the narrow leg clears whole, so the closure row below pins that outcome,
//! not the line (`write_is_frame_private` says why the line is kept regardless).
//!
//! Zero emission (A-G9): every fixture here dumps and nothing else.
//!
//! [`write_is_frame_private`]: ../../steins_infer/shapes/fn.write_is_frame_private.html

use std::collections::HashMap;

use steins_domain::Fact;
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

#[derive(Default)]
struct Mock {
    facts: HashMap<String, Fact>,
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
}

/// The `dumped type: ` prefix every `debug.type` body carries — stripped so the
/// assertions below read as the answer alone.
const DUMPED: &str = "dumped type: ";

/// Every `debug.type` answer the source produces, in source order, asserting on
/// the way that it produced NO other finding (A-G9).
fn types(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let ds: Vec<Diagnostic> = check_with(&tree, &[], "t.php", &mut Mock::default())
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect();
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "the fixture emitted a finding: {other:?}");
    ds.iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.strip_prefix(DUMPED).unwrap_or(&d.message).to_owned())
        .collect()
}

/// The single dump of a one-dump function body.
fn dump_body(body: &str) -> String {
    let got = types(&format!("<?php\nfunction f(): void {{ {body} }}\n"));
    assert_eq!(got.len(), 1, "expected exactly one dump, got {got:?}");
    got.into_iter().next().unwrap()
}

/// The single dump of a body whose function signature is written out in full — the
/// by-ref-parameter and closure rows need one.
fn dump_sig(sig: &str, body: &str) -> String {
    let got = types(&format!("<?php\nfunction f({sig}): void {{ {body} }}\n"));
    assert_eq!(got.len(), 1, "expected exactly one dump, got {got:?}");
    got.into_iter().next().unwrap()
}

// ---------------------------------------------------------------------------
// The exposure leg, pinned where a poisoned frame still binds something.
// ---------------------------------------------------------------------------

/// A closure scope keeps a declared return beside a `$y = &$x` (a named function's
/// poisoned env is empty; a closure's is not), so this is the one frame where a
/// poisoned scope has something to lose across an offset write. It loses it: the
/// second dump is `unknown`. Today that outcome does not depend on the exposure
/// leg alone — the fact sits in a store lane the narrow leg clears whole — so this
/// row pins the outcome, and the leg's own reason is on `write_is_frame_private`.
#[test]
fn an_offset_write_in_a_poisoned_closure_scope_stays_total() {
    let src = "<?php
/** @return list<non-empty-array<mixed>> */
function get_list(): array { return [['a' => 1]]; }
function (): void {
    $foo = get_list();
    $x = 1;
    $y = &$x;
    \\PHPStan\\dumpType($foo);
    $b = ['k' => 1];
    $b['k'] = 2;
    \\PHPStan\\dumpType($foo);
};
";
    assert_eq!(
        types(src),
        vec!["list<non-empty-array<mixed>> (asserted)".to_owned(), "unknown".to_owned()],
        "the frame is aliased, so the write clears everything"
    );
}

// ---------------------------------------------------------------------------
// The statement's OTHER writers: the total barrier used to mask them.
// ---------------------------------------------------------------------------

/// `const_key_offset_path` admits any expression as the outermost key, and the
/// right-hand side may carry an assignment or increment of its own. Each of these
/// writes `$s`, and each answered the pre-write value before the statement named
/// its writers. `php -r` for the first: `$s = "foo"; $b = ["k" => 1];
/// $b[$s = "bar"] = 1; var_dump($s);` prints `string(3) "bar"`.
#[test]
fn a_writer_inside_the_key_or_the_value_of_an_offset_write_is_swept() {
    let by_ref = "function g(string &$s): int { $s = 'bar'; return 1; }\n";
    for (setup, stmt) in [
        ("$s = 'foo'; $b = ['k' => 1];", "$b[$s = 'bar'] = 1;"),
        ("$s = 'foo'; $b = ['k' => 1];", "$b[g($s)] = 1;"),
        ("$s = 'foo'; $b = ['k' => ['j' => 1]];", "$b['k'][$s = 'j'] = 1;"),
        ("$s = 1; $b = ['k' => 1];", "$b['k'] = $s++;"),
        ("$s = 'foo'; $b = ['k' => 1];", "$b['k'] = $s = 'bar';"),
        ("$s = 'foo'; $b = ['k' => 1];", "$b['k'] = $s .= 'x';"),
        ("$s = 1; $b = [1];", "$b[] = $s++;"),
    ] {
        let src = format!(
            "<?php\n{by_ref}function f(): void {{ {setup} {stmt} \\PHPStan\\dumpType($s); }}\n"
        );
        let got = types(&src);
        assert_eq!(got, vec!["unknown".to_owned()], "`{stmt}` writes `$s`");
    }
}

// ---------------------------------------------------------------------------
// The rows that MUST NOT move: an aliasing carrier keeps the barrier total.
//
// `bug-14333.php` is the fixture whose whole subject is reference propagation
// through an array literal, and it is the fixture that most clearly demonstrates
// what the total barrier costs — and the one that must keep paying it.
// ---------------------------------------------------------------------------

/// `bug-14333.php:16` in four lines. `php -r '$a = 1; $b = ["key" => &$a];
/// $b["key"] = 42; var_dump($a);'` prints `int(42)`.
#[test]
fn a_reference_in_an_array_literal_keeps_the_barrier_total() {
    let got = dump_body("$a = 1; $b = ['key' => &$a]; $b['key'] = 42; \\PHPStan\\dumpType($a);");
    assert_eq!(got, "unknown", "a keyed by-ref element must keep the write observable: {got}");
}

/// `bug-14333.php:32`. `php -r '$a = 1; $c = "test"; $b = [&$a, "normal", &$c];
/// $b[0] = 2; $b[2] = "bar"; var_dump($a, $c);'` prints `int(2)` and `string(3) "bar"`.
#[test]
fn a_positional_reference_element_keeps_the_barrier_total() {
    let got =
        dump_body("$a = 1; $c = 'x'; $b = [&$a, 'n', &$c]; $b[0] = 2; \\PHPStan\\dumpType($c);");
    assert_eq!(got, "unknown", "a positional by-ref element aliases too: {got}");
}

/// `bug-13789.php`'s scope: a by-ref `foreach` whose alias outlives the loop —
/// which is why PHP's own idiom is to `unset($row)` after one.
/// `php -r '$r = [[1],[2]]; foreach ($r as &$v) {} $v[] = 9; var_dump($r[1]);'`
/// prints `array(2) { [0]=> int(2) [1]=> int(9) }`.
#[test]
fn a_by_ref_foreach_keeps_the_barrier_total() {
    let got = dump_body(
        "$s = 'foo'; $r = [[1]]; foreach ($r as &$v) {} $b = ['k' => 1]; $b['k'] = 2; \
         \\PHPStan\\dumpType($s);",
    );
    assert_eq!(got, "unknown", "a by-ref foreach aliases the frame: {got}");
}

#[test]
fn a_reference_assignment_keeps_the_barrier_total() {
    let got = dump_body("$s = 'foo'; $a = 1; $c = &$a; $b = ['k' => 1]; $b['k'] = 2; \\PHPStan\\dumpType($s);");
    assert_eq!(got, "unknown", "`$c = &$a` is the oldest carrier of all: {got}");
}

#[test]
fn a_global_declaration_keeps_the_barrier_total() {
    let got =
        dump_body("global $g; $s = 'foo'; $b = ['k' => 1]; $b['k'] = 2; \\PHPStan\\dumpType($s);");
    assert_eq!(got, "unknown", "a `global` local is an alias of a global cell: {got}");
}

#[test]
fn an_extract_call_keeps_the_barrier_total() {
    let got = dump_sig(
        "array $r",
        "$s = 'foo'; extract($r); $b = ['k' => 1]; $b['k'] = 2; \\PHPStan\\dumpType($s);",
    );
    assert_eq!(got, "unknown", "`extract` mints names over the scope: {got}");
}

#[test]
fn a_variable_variable_keeps_the_barrier_total() {
    let got = dump_sig(
        "string $n",
        "$s = 'foo'; $$n = 1; $b = ['k' => 1]; $b['k'] = 2; \\PHPStan\\dumpType($s);",
    );
    assert_eq!(got, "unknown", "`$$n` can name any binding in the frame: {got}");
}

/// `constantTypes.php:117`–`:129`'s carrier, `use (&$x)` at `:104`.
#[test]
fn a_by_ref_closure_capture_keeps_the_barrier_total() {
    let got = dump_body(
        "$s = 'foo'; $n = 1; $f = function () use (&$n) { $n++; }; $b = ['k' => 1]; \
         $b['k'] = 2; \\PHPStan\\dumpType($s);",
    );
    assert_eq!(got, "unknown", "a by-ref capture poisons both sides of the capture: {got}");
}

/// A closure body is its own scope with its own give-up list, and the enclosing
/// walk deliberately does not descend into it — so the carrier has to be found
/// twice, once on each side. `bug-13789.php`'s whole subject sits inside a
/// `function (): void {…}`, which is why this row exists separately.
#[test]
fn a_closure_scope_finds_its_own_reference_binding() {
    let got = types(
        "<?php\n$f = function (): void { $s = 'foo'; $r = [[1]]; foreach ($r as &$v) {} \
         $b = ['k' => 1]; $b['k'] = 2; \\PHPStan\\dumpType($s); };\n",
    );
    assert_eq!(got.len(), 1, "expected one dump, got {got:?}");
    assert_eq!(got[0], "unknown", "the closure's own scope must be poisoned too: {got:?}");
}

/// The target leg's first decline: a superglobal root is interpreter-global
/// surface however local the syntax looks (ADR-0055 amendment).
#[test]
fn a_superglobal_target_keeps_the_barrier_total() {
    let got = dump_body("$s = 'foo'; $_SESSION['k'] = 1; \\PHPStan\\dumpType($s);");
    assert_eq!(got, "unknown", "a write into `$_SESSION` is not frame-private: {got}");
}

/// The target leg's second decline: a by-ref parameter's cell belongs to the
/// caller, and two by-ref parameters can be the same cell (`f($x, $x)`).
#[test]
fn a_by_ref_parameter_target_keeps_the_barrier_total() {
    let got = dump_sig("array &$p", "$s = 'foo'; $p['k'] = 1; \\PHPStan\\dumpType($s);");
    assert_eq!(got, "unknown", "writing a by-ref parameter is caller-observable: {got}");
}

// ---------------------------------------------------------------------------
// The retention rule: in a frame with no carrier, a write to `$b` clears `$b`.
// ---------------------------------------------------------------------------

/// The other side of the pin above, on the same store — the only thing that
/// changed is the `&`.
#[test]
fn a_plain_element_lets_the_bystander_survive() {
    let got = dump_body("$a = 1; $b = ['key' => 1]; $b['key'] = 42; \\PHPStan\\dumpType($a);");
    assert_eq!(got, "1", "no reference in the frame, so nothing saw the write: {got}");
}

#[test]
fn an_append_lets_the_bystander_survive() {
    let got = dump_body("$s = 'foo'; $b = [1]; $b[] = 2; \\PHPStan\\dumpType($s);");
    assert_eq!(got, "'foo'", "an append names its base and nothing else: {got}");
}

#[test]
fn an_offset_unset_lets_the_bystander_survive() {
    let got = dump_body("$s = 'foo'; $b = ['k' => 1]; unset($b['k']); \\PHPStan\\dumpType($s);");
    assert_eq!(got, "'foo'", "`unset` at a literal key names one slot of one base: {got}");
}

/// A write at a key nobody can name (issue #636) weakens the base's own shape and
/// still may not reach past it.
#[test]
fn a_write_at_an_unnamed_key_lets_the_bystander_survive() {
    let got = dump_sig(
        "int $i",
        "$s = 'foo'; $b = ['k' => 1]; $b[$i] = 2; \\PHPStan\\dumpType($s);",
    );
    assert_eq!(got, "'foo'", "an unnameable key is still a key of one base: {got}");
}

/// The issue's headline witness, `array-functions.php:178`–`:182` in miniature:
/// the second append used to throw away the first one's restore, because
/// `$one` is not `$two`.
#[test]
fn a_second_append_no_longer_eats_the_first_ones_restore() {
    let got = types(
        "<?php\nfunction f(): void { $one = ['a']; $two = ['b']; $one[] = 'c'; $two[] = 'd'; \
         \\PHPStan\\dumpType($one); \\PHPStan\\dumpType($two); }\n",
    );
    assert_eq!(got.len(), 2, "expected two dumps, got {got:?}");
    assert!(got[0].contains("'c'"), "the first append's own restore survived the second: {got:?}");
    assert!(got[1].contains("'d'"), "the second append restored its own base: {got:?}");
}

// ---------------------------------------------------------------------------
// What the narrow leg still refuses (the issue's step 4).
// ---------------------------------------------------------------------------

/// `refs`/`heap`/`members`/`narrowed` go either way: `$b['k'] = $v` on an
/// `ArrayAccess` receiver runs `offsetSet`, and nothing here bounds what that
/// body reaches. `binary.php:412` lives here and is deliberately left here.
#[test]
fn object_identity_does_not_survive_the_narrow_leg() {
    let got = types(
        "<?php\nclass Foo {}\nfunction f(): void { $o = new Foo(); $b = ['k' => 1]; \
         $b['k'] = 2; \\PHPStan\\dumpType($o); }\n",
    );
    assert_eq!(got.len(), 1, "expected one dump, got {got:?}");
    assert_eq!(got[0], "unknown", "the heap lane is still cleared: {got:?}");
}

/// A plain `StmtKind::Barrier` names no target, so it cannot be narrowed at all —
/// the variant is a unit and the lowering threw the lvalue away on the way in.
/// A depth-3 offset path is one of the statements that take it.
#[test]
fn a_plain_barrier_still_clears_everything() {
    let got = dump_body(
        "$s = 'foo'; $b = ['k' => ['j' => ['i' => 1]]]; $b['k']['j']['i'] = 2; \
         \\PHPStan\\dumpType($s);",
    );
    assert_eq!(got, "unknown", "a `Barrier` still names nothing: {got}");
}

/// A destructuring assignment writes an unbounded set of targets the lowering
/// deliberately does not model (issue #288), so it keeps the total clear too.
#[test]
fn a_destructuring_assignment_still_clears_everything() {
    let got =
        dump_body("$s = 'foo'; $r = [1, 2]; [$x, $y] = $r; \\PHPStan\\dumpType($s);");
    assert_eq!(got, "unknown", "`Destructure` is leg 3's problem, not this one's: {got}");
}
