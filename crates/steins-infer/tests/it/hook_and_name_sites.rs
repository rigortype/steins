//! Name operands, offset-write values and property hooks outside an explicit
//! `$this->p = …` (ADR-0099 §4.2, §4.3, issues #880 and #875): the witness table of
//! the S4 slice, each row run on PHP 8.5.11. A row reads in both lanes, so a case
//! states the effect lane's gap kinds and the throw lane's, sorted.
//!
//! The shared prelude's classes: `S` and `S2` convert to a string (with output),
//! `N` does not; `P` is a plain class; `HEx` hooks `$message`'s `set`, `GEx` its
//! `get`, `PlainEx` hooks nothing; `Promo` promotes a parameter with a `set` hook.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

const PRELUDE: &str = "<?php\n\
    class S { public function __toString(): string { echo '[S]'; return 'name'; } }\n\
    final class FinalS { public function __toString(): string { echo '[F]'; return 'name'; } }\n\
    class N {}\n\
    final class FinalN {}\n\
    class S2 { public function __toString(): string { echo '[S2]'; return 'sname'; } }\n\
    class P { public $name = 'v'; public static $sname = 'sv'; }\n\
    class HEx extends RuntimeException {\n\
        protected $message { set($v) { echo '[HEx set]'; $this->message = $v; } }\n\
    }\n\
    class GEx extends RuntimeException {\n\
        protected $message { get { echo '[GEx get]'; return 'g'; } }\n\
    }\n\
    class PlainEx extends RuntimeException {}\n\
    class Promo {\n\
        public function __construct(public string $p { set(string $v) { echo '[Promo set]'; $this->p = $v; } }) {}\n\
    }\n\
    class PromoPlain { public function __construct(public string $p) {} }\n";

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    assert!(tree.parse_errors().is_empty(), "{:?}\n{src}", tree.parse_errors());
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// The gap kinds of `symbol` in the effect lane and in the throw lane, each sorted.
fn lanes(code: &str, symbol: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let s = summary(&format!("{PRELUDE}{code}\n"), symbol);
    let sorted = |mut kinds: Vec<&'static str>| {
        kinds.sort_unstable();
        kinds
    };
    assert_eq!(s.exhaustive, s.gaps.is_empty(), "{s:?}");
    assert_eq!(s.throws_exhaustive, s.throws_gaps.is_empty(), "{s:?}");
    (sorted(s.gaps), sorted(s.throws_gaps))
}

/// `symbol` reads exhaustive in both lanes.
fn exhaustive(code: &str, symbol: &str) {
    assert_eq!(lanes(code, symbol), (vec![], vec![]), "{symbol}\n{code}");
}

/// `symbol` has exactly `kind` as a gap in both lanes.
fn gap(code: &str, symbol: &str, kind: &'static str) {
    assert_eq!(lanes(code, symbol), (vec![kind], vec![kind]), "{symbol}\n{code}");
}

/// `symbol` has `kind` among its gaps in both lanes, whatever else it has.
fn has_gap(code: &str, symbol: &str, kind: &'static str) {
    let (effects, throws) = lanes(code, symbol);
    assert!(effects.contains(&kind) && throws.contains(&kind), "{symbol}: {effects:?} {throws:?}\n{code}");
}

/// `symbol` has no `kind` gap in either lane, whatever else it has.
fn lacks_gap(code: &str, symbol: &str, kind: &'static str) {
    let (effects, throws) = lanes(code, symbol);
    assert!(!effects.contains(&kind) && !throws.contains(&kind), "{symbol}: {effects:?} {throws:?}\n{code}");
}

const TO_STRING: &str = "operator-to-string";
const PROPERTY: &str = "operator-magic-property";

// ---- 4.1–4.3 name operands -----------------------------------------------------

#[test]
fn a_dynamic_property_name_that_may_be_an_object_is_a_to_string_gap() {
    // 4.1: `$n` is a written local, so nothing names its class.
    gap("function f() { $n = new S; return (new P)->$n; }", "f", TO_STRING);
    gap("function f($n) { return (new P)->$n; }", "f", TO_STRING);
    gap("function f(S $n) { return (new P)->$n; }", "f", TO_STRING);
    gap("function f($n) { return (new P)->{$n}; }", "f", TO_STRING);
    has_gap("function f($n, P $p) { $p->$n = 1; return 1; }", "f", TO_STRING);
    has_gap("function f($n, P $p) { return isset($p->$n); }", "f", TO_STRING);
}

#[test]
fn a_variable_variable_name_that_may_be_an_object_is_a_to_string_gap() {
    // 4.2
    gap("function f() { $n = new S; $name = 'x'; return $$n; }", "f", TO_STRING);
    gap("function f($n) { $x = 1; return ${$n}; }", "f", TO_STRING);
    gap("function f($n) { $$n = 1; return 1; }", "f", TO_STRING);
}

#[test]
fn a_static_property_name_that_may_be_an_object_is_a_to_string_gap_beside_the_state_construct() {
    // 4.3: the effect lane reads `P::$$n` as a state construct as well.
    let (effects, throws) = lanes("function f() { $n = new S2; return P::$$n; }", "f");
    assert_eq!(effects, ["operator-to-string", "state-construct"]);
    assert_eq!(throws, [TO_STRING]);
}

#[test]
fn a_name_shown_not_to_be_an_object_stays_exhaustive() {
    // 4.9: a `string` parameter, a literal, a concatenation.
    exhaustive("function f(string $n) { return (new P)->$n; }", "f");
    exhaustive("function f() { return (new P)->{'na' . 'me'}; }", "f");
    exhaustive("function f() { return (new P)->name; }", "f");
    lacks_gap("function f(string $n) { return P::$sname; }", "f", TO_STRING);
    // An exact class that is not `Stringable`: the engine raises, no user code runs.
    exhaustive("function f() { return (new P)->{new FinalN}; }", "f");
}

#[test]
fn a_frame_holding_a_variable_variable_shows_no_name_of_its_own() {
    // `$$m = …` can rebind any name of the frame (ADR-0001's give-up list), so even a
    // `string` parameter is not shown to hold a string there: the name is a gap, as
    // every other operator site over a variable of such a frame is.
    gap("function f(string $n) { $x = 1; return $$n; }", "f", TO_STRING);
    has_gap("function f(string $n) { return P::$$n; }", "f", TO_STRING);
}

#[test]
fn a_name_that_is_an_exact_stringable_is_an_edge() {
    let src = "function f() { return (new P)->{new FinalS}; }";
    exhaustive(src, "f");
    let s = summary(&format!("{PRELUDE}{src}\n"), "f");
    assert_eq!(s.labels, ["io.output.buffer"]);
}

// ---- 4.4–4.8 offset-write values -----------------------------------------------

#[test]
fn a_string_offset_write_of_an_exact_stringable_is_an_edge_to_its_conversion() {
    // 4.4
    let src = "function f() { $s = 'abc'; $s[0] = new S; return $s; }";
    exhaustive(src, "f");
    assert_eq!(summary(&format!("{PRELUDE}{src}\n"), "f").labels, ["io.output.buffer"]);
}

#[test]
fn an_offset_write_of_a_value_that_may_convert_into_a_container_that_may_be_a_string_is_a_gap() {
    // 4.5: a `string` parameter, and `$this->buf` declared `string`.
    gap("function f(string $s, S $o) { $s[0] = $o; return $s; }", "f", TO_STRING);
    let prop = "class H { private string $buf = 'abc'; function w(S $o) { $this->buf[0] = $o; } }";
    let (effects, throws) = lanes(prop, "H::w");
    assert_eq!(effects, ["operator-to-string", "state-construct"]);
    assert_eq!(throws, [TO_STRING]);
    // The container may be anything: untyped, an element, a call result.
    has_gap("function f($c, S $o) { $c[0] = $o; return 1; }", "f", TO_STRING);
    has_gap("function f(array $a, S $o) { $a['x']['y'] = $o; return 1; }", "f", TO_STRING);
}

#[test]
fn an_offset_write_into_a_container_shown_not_to_be_a_string_runs_nothing() {
    // 4.6: an array parameter and a property declared `array`.
    exhaustive("function f(array $a, S $o) { $a[0] = $o; return 1; }", "f");
    let prop = "class H { private array $items = []; function w(S $o) { $this->items[0] = $o; } }";
    assert_eq!(lanes(prop, "H::w"), (vec!["state-construct"], vec![]));
    // The same, found by what the frame writes: a local only ever an array, `null`
    // or an object, and a parameter whose declared type admits no string.
    exhaustive("function f(S $o) { $a = []; $a[0] = new S; $a['k'] = $o; return count($a); }", "f");
    exhaustive("function f(S $o) { $a = null; $a[0] = $o; return $a; }", "f");
    // An `ArrayAccess` object's `offsetSet` is the ArrayAccess family's question, not a conversion.
    let object = "function f(ArrayObject $c, S $o) { $c['k'] = $o; return 1; }";
    assert!(!lanes(object, "f").0.contains(&TO_STRING));
}

#[test]
fn a_property_container_is_a_non_string_only_by_its_declared_type() {
    let class = |ty: &str| {
        format!("class H {{ private {ty} $buf; function w(S $o) {{ $this->buf[0] = $o; }} }}")
    };
    for ty in ["array", "?array", "array|null", "N", "?N", "int", "iterable", "object"] {
        lacks_gap(&class(ty), "H::w", TO_STRING);
    }
    // A type that admits a string, or none: a conversion may happen.
    for ty in ["string", "?string", "array|string", "mixed", "callable", ""] {
        has_gap(&class(ty), "H::w", TO_STRING);
    }
    // A hooked property is whatever its hook returns.
    let hooked = "class H { public array $buf { get => []; } function w(S $o) { $this->buf[0] = $o; } }";
    has_gap(hooked, "H::w", TO_STRING);
}

#[test]
fn a_local_that_may_hold_a_string_is_a_container_that_may_be_one() {
    gap("function f(S $o) { $a = 'abc'; $a[0] = $o; return $a; }", "f", TO_STRING);
    has_gap("function f(S $o, $x) { $a = []; $a = $x; $a[0] = $o; return 1; }", "f", TO_STRING);
    gap("function f(S $o) { $a = []; $a[0] = $o; $a = 'abc'; return $a; }", "f", TO_STRING);
    has_gap("function f(S $o, array $x) { foreach ($x as $a) { $a[0] = $o; } return 1; }", "f", TO_STRING);
    // A call that may take the variable by reference rebinds it.
    let src = "function take(&$v) { $v = 'abc'; }\n\
        function f(S $o) { $a = []; take($a); $a[0] = $o; return $a; }";
    has_gap(src, "f", TO_STRING);
}

#[test]
fn an_offset_write_that_is_an_exact_not_stringable_or_an_object_free_value_is_not_a_conversion() {
    // 4.7: an exact class with no `__toString` raises an `Error`, no user code runs.
    exhaustive("function f() { $s = 'abc'; $s[0] = new N; return $s; }", "f");
    exhaustive("function f() { $s = 'abc'; $s[0] = new FinalN; return $s; }", "f");
    // 4.8: an integer, a literal, a concatenation, an array.
    exhaustive("function f(string $s, int $i) { $s[0] = $i; return $s; }", "f");
    exhaustive("function f(string $s) { $s[0] = 'z'; $s[1] = 1 . 'x'; return $s; }", "f");
    exhaustive("function f(string $s, S $o) { $s[0] = [$o]; return $s; }", "f");
    // An append on a string is a fatal error, never a conversion.
    exhaustive("function f(string $s, S $o) { $s[] = $o; return $s; }", "f");
}

#[test]
fn a_destructuring_or_foreach_target_that_is_an_offset_of_a_possible_string_is_a_gap() {
    gap("function f(string $s, array $a) { [$s[0]] = $a; return $s; }", "f", TO_STRING);
    gap("function f(string $s, array $a) { foreach ($a as $s[0]) {} return $s; }", "f", TO_STRING);
    exhaustive("function f(array $c, array $a) { [$c[0]] = $a; return 1; }", "f");
}

// ---- 4.10, 4.11 promotion ------------------------------------------------------

#[test]
fn a_hooked_promoted_parameter_is_a_magic_property_gap_in_the_constructors_own_row() {
    // 4.10: the constructor's row, and the caller through its edge.
    gap("", "Promo::__construct", PROPERTY);
    gap("function f() { new Promo('lit'); return 1; }", "f", PROPERTY);
    // 4.11
    exhaustive("", "PromoPlain::__construct");
    exhaustive("function f() { new PromoPlain('lit'); return 1; }", "f");
    // Only the hooked one of several.
    let two = "class Two { public function __construct(public int $a, public int $b { set => 1; }, int $c) {} }";
    gap(two, "Two::__construct", PROPERTY);
}

// ---- 4.12–4.15 hooks under an engine class --------------------------------------

#[test]
fn a_project_exception_that_hooks_a_property_is_a_gap_at_its_inherited_constructor() {
    // 4.12, in both lanes.
    gap("function f() { return new HEx('lit'); }", "f", PROPERTY);
    // 4.13
    exhaustive("function f() { return new PlainEx('lit'); }", "f");
    // The hook may sit on any project class of the chain, and `parent::__construct`
    // reaches the engine's constructor on `$this`, which is the hooking class.
    let chain = "class Mid extends HEx {}\nfunction f() { return new Mid('lit'); }";
    gap(chain, "f", PROPERTY);
    let forwarded = "class H3 extends RuntimeException {\n\
        protected $message { set($v) { $this->message = $v; } }\n\
        function __construct() { parent::__construct('x'); }\n}";
    gap(forwarded, "H3::__construct", PROPERTY);
    let plain = "class P3 extends RuntimeException { function __construct() { parent::__construct('x'); } }";
    exhaustive(plain, "P3::__construct");
}

#[test]
fn the_final_accessor_of_an_engine_exception_runs_a_hook_the_chain_declares() {
    // 4.14: effect lane only beside the missing throw row, the `new` is a gap in both.
    let (effects, throws) = lanes("function f() { return (new GEx('lit'))->getMessage(); }", "f");
    assert_eq!(effects, [PROPERTY]);
    assert_eq!(throws, ["no-throw-row", PROPERTY]);
    // A chain with no hook: the accessor row stands.
    let (effects, _) = lanes("function f() { return (new PlainEx('lit'))->getMessage(); }", "f");
    assert_eq!(effects, Vec::<&str>::new());
}

#[test]
fn a_declared_throwable_is_a_gap_when_some_class_that_may_stand_in_hooks_a_property() {
    // 4.15: `GEx` is in the universe; the throw lane keeps its declared-receiver gap.
    let (effects, throws) = lanes("function g(Throwable $e) { return $e->getMessage(); }", "g");
    assert_eq!(effects, [PROPERTY]);
    assert_eq!(throws, ["declared-receiver"]);
    // Only a class that is a `RuntimeException` can stand in for this receiver.
    let own = "class Mine extends LogicException { function m() { return $this->getMessage(); } }";
    let (effects, _) = lanes(own, "Mine::m");
    assert_eq!(effects, Vec::<&str>::new());
    let (effects, _) = lanes("function g(RuntimeException $e) { return $e->getMessage(); }", "g");
    assert_eq!(effects, [PROPERTY]);
    let (effects, _) = lanes("function g(LogicException $e) { return $e->getMessage(); }", "g");
    assert_eq!(effects, Vec::<&str>::new());
}

#[test]
fn a_universe_with_no_hooking_exception_leaves_the_accessor_exhaustive() {
    let src = "<?php\nclass PlainEx extends RuntimeException {}\n\
        function g(Throwable $e) { return $e->getMessage(); }\n";
    let s = summary(src, "g");
    assert!(s.exhaustive, "{s:?}");
}
