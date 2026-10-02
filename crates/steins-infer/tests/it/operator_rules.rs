//! The operator sites resolve against the operand's class (ADR-0099 §4.3, §4.4,
//! issue #859): converting an object to a string, fetching or storing a property
//! on it, indexing it, iterating it and cloning it run the user method the class
//! names, and the site is a coverage gap where no class can be pinned. Each
//! family is read for an operand shown not to be an object, an exact class with
//! the method and without, a bound (a non-final `$this`, an interface, a
//! non-final class), and an operand nothing is known of. Both lanes read the one
//! resolution, so every case asserts the two agree.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

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

/// The classes every case may name: a final class converting to a string with an
/// effect, one whose conversion throws, a non-final one, a final one with no
/// magic at all, and an interface.
const CLASSES: &str = "<?php\n\
    final class Name { public function __toString(): string { echo 'x'; return 'x'; } }\n\
    final class Boom { public function __toString(): string { throw new \\RuntimeException('x'); } }\n\
    class Open { public function __toString(): string { return 'x'; } }\n\
    final class Plain { public $a = 1; }\n\
    interface Shape {}\n";

fn file(extra: &str, function: &str) -> String {
    format!("{CLASSES}{extra}\n{function}\n")
}

/// `f`'s gap kinds in the effect lane and in the throw lane, which must agree:
/// the operator resolution is one answer.
fn gaps(src: &str, symbol: &str) -> Vec<&'static str> {
    let s = summary(src, symbol);
    assert_eq!(s.gaps, s.throws_gaps, "the two lanes read one resolution: {s:?}\n{src}");
    assert_eq!(s.exhaustive, s.gaps.is_empty(), "{s:?}");
    assert_eq!(s.throws_exhaustive, s.throws_gaps.is_empty(), "{s:?}");
    s.gaps
}

/// Only the operator resolver's kinds, from either lane (the lanes agree on them).
fn operator_gaps(src: &str, symbol: &str) -> Vec<&'static str> {
    let s = summary(src, symbol);
    let operator = |kinds: &[&'static str]| -> Vec<&'static str> {
        kinds.iter().copied().filter(|kind| kind.starts_with("operator-")).collect()
    };
    assert_eq!(operator(&s.gaps), operator(&s.throws_gaps), "{s:?}");
    operator(&s.gaps)
}

fn covered(src: &str, symbol: &str) {
    let found = gaps(src, symbol);
    assert!(found.is_empty(), "{symbol} should be covered, has {found:?}\n{src}");
}

fn gap(src: &str, symbol: &str, kind: &str) {
    assert_eq!(gaps(src, symbol), [kind], "{symbol}\n{src}");
}

/// `f`'s proven effect labels and escaping throw classes.
fn runs(src: &str, symbol: &str) -> (Vec<String>, Vec<String>) {
    let s = summary(src, symbol);
    (s.labels, s.throws)
}

const TO_STRING: &str = "operator-to-string";
const PROPERTY: &str = "operator-magic-property";
const OFFSET: &str = "operator-array-access";
const ITERATION: &str = "operator-iteration";
const CLONE: &str = "operator-clone";

// ---- ToString ---------------------------------------------------------------

#[test]
fn a_string_conversion_of_a_value_shown_not_to_be_an_object_runs_nothing() {
    covered(&file("", "function f(string $s, int $i) { return 'a' . $s . $i; }"), "f");
    covered(&file("", "function f() { return (string) 1 . 'x' . 2.5 . true; }"), "f");
    covered(&file("", "function f() { $x = 1; return \"v: {$x}\"; }"), "f");
    // A comparison reads an array's elements, so an array is not enough there.
    gap(&file("", "function f(array $a) { return $a == 'x'; }"), "f", TO_STRING);
}

#[test]
fn an_exact_class_with_to_string_is_an_edge_carrying_its_labels_and_throws() {
    let named = file("", "function f(Name $o) { return 'a' . $o; }");
    covered(&named, "f");
    assert_eq!(runs(&named, "f").0, ["io.output.buffer"]);
    let thrown = file("", "function f(Boom $o) { return (string) $o; }");
    covered(&thrown, "f");
    assert_eq!(runs(&thrown, "f").1, ["RuntimeException"]);
    // `new Name` names its class exactly, wherever the conversion sits.
    let fresh = file("", "function f() { return \"x\" . new Name(); }");
    covered(&fresh, "f");
    assert_eq!(runs(&fresh, "f").0, ["io.output.buffer"]);
}

#[test]
fn an_exact_class_without_to_string_runs_nothing() {
    covered(&file("", "function f(Plain $o) { return 'a' . $o; }"), "f");
    covered(&file("", "function f(?Plain $o) { return \"{$o}\"; }"), "f");
}

#[test]
fn a_bound_class_is_a_gap_unless_its_to_string_is_final() {
    gap(&file("", "function f(Open $o) { return 'a' . $o; }"), "f", TO_STRING);
    gap(&file("", "function f(Shape $o) { return 'a' . $o; }"), "f", TO_STRING);
    gap(&file("", "function f($o) { return 'a' . $o; }"), "f", TO_STRING);
    let sealed = "class Fixed { final public function __toString(): string { echo 'f'; return 'x'; } }";
    let src = file(sealed, "function f(Fixed $o) { return 'a' . $o; }");
    covered(&src, "f");
    assert_eq!(runs(&src, "f").0, ["io.output.buffer"]);
    // `$this` in a non-final class is a bound; in a final class it is exact.
    let open = "class K { public function m() { return 'a' . $this; } }";
    gap(&file(open, ""), "K::m", TO_STRING);
    let closed = "final class K { public function m() { return 'a' . $this; } }";
    covered(&file(closed, ""), "K::m");
}

#[test]
fn every_string_conversion_form_is_a_site() {
    for body in [
        "$s = 'a'; $s .= $o; return $s;",
        "return \"x{$o}\";",
        "return <<<T\n{$o}\nT;",
        "return (string) $o;",
        "return $o == 'x';",
        "return $o <=> 'x';",
        "switch ($o) { case 'a': return 1; } return 0;",
    ] {
        gap(&file("", &format!("function f(Open $o) {{ {body} }}")), "f", TO_STRING);
    }
}

#[test]
fn a_union_is_ruled_out_only_when_every_member_is() {
    covered(&file("", "function f(Plain|int $o) { return 'a' . $o; }"), "f");
    let both = file("", "function f(Name|Plain $o) { return 'a' . $o; }");
    covered(&both, "f");
    assert_eq!(runs(&both, "f").0, ["io.output.buffer"]);
    gap(&file("", "function f(Name|Open $o) { return 'a' . $o; }"), "f", TO_STRING);
    // A member the lowering does not model leaves the operand unknown.
    gap(&file("", "function f(array|Plain $o) { return 'a' . $o; }"), "f", TO_STRING);
}

#[test]
fn a_comparison_with_a_non_string_scalar_converts_nothing() {
    covered(&file("", "function f(Open $o) { return $o == 1; }"), "f");
    covered(&file("", "function f(Open $o) { return $o == null; }"), "f");
}

// ---- MagicProp --------------------------------------------------------------

const PROPERTY_CLASSES: &str = "\
    final class Lazy { public function __get($n) { echo 'g'; return 1; }\n\
        public function __set($n, $v) { throw new \\LogicException('s'); } }\n\
    final class Sealed { public int $x = 1; private $hidden = 2;\n\
        public function r() { return $this->x; }\n\
        public function u() { return $this->nope; }\n\
        public function w() { $this->nope = 1; return isset($this->nope); } }\n\
    class Base { public int $x = 1; protected $y = 2; private $z = 3;\n\
        public function r() { return $this->x + $this->y + $this->z; }\n\
        public function u() { return $this->nope; } }\n\
    class Parented { public int $x = 1; public function r() { return $this->x; } }\n\
    class Lazier extends Parented { public function __get($n) { return 1; } }\n\
    class Wide { public int $x = 1; public function __get($n) { return 1; }\n\
        public function r() { return $this->x; } }\n\
    class Orphan extends \\Missing\\Vendor { public $p; public function r() { return $this->p; } }\n\
    class Hooked { public int $h { get => 1; } public function r() { return $this->h; } }\n\
    final class HookedFinal { public int $h { get => 1; } public function r() { return $this->h; } }\n";

#[test]
fn a_declared_visible_property_of_a_bound_class_runs_nothing_behind_the_universe_gate() {
    // `Base` has no magic anywhere on its chain or below it.
    covered(&file(PROPERTY_CLASSES, ""), "Base::r");
    // A subclass elsewhere in the universe adds `__get`.
    gap(&file(PROPERTY_CLASSES, ""), "Parented::r", PROPERTY);
    // The class declares `__get` itself.
    gap(&file(PROPERTY_CLASSES, ""), "Wide::r", PROPERTY);
    // A parent outside the universe: the chain is not closed.
    gap(&file(PROPERTY_CLASSES, ""), "Orphan::r", PROPERTY);
}

#[test]
fn a_subclass_importing_a_trait_or_written_anonymously_closes_the_universe_gate() {
    // A trait's body is not lowered, so a subclass using one may bring `__get`.
    let traited = "trait Magic { public function __get($n) { return 1; } }\n\
        class Base { public int $x = 1; public function r() { return $this->x; } }\n\
        class Sub extends Base { use Magic; }";
    gap(&file(traited, ""), "Base::r", PROPERTY);
    // An anonymous class is listed by no index; one extending the class counts,
    // whatever its body declares.
    let anonymous = "class Base3 { public int $x = 1; public function r() { return $this->x; } }\n\
        class Other { public int $x = 1; public function r() { return $this->x; } }\n\
        function mk() { return new class extends Base3 { public function __get($n) { return 1; } }; }";
    gap(&file(anonymous, ""), "Base3::r", PROPERTY);
    // A class nothing anonymous extends is untouched.
    covered(&file(anonymous, ""), "Other::r");
    // So is one an anonymous class implements only an interface of.
    let sibling = "interface Tag {}\n\
        class Base4 { public int $x = 1; public function r() { return $this->x; } }\n\
        function mk() { return new class implements Tag {}; }";
    covered(&file(sibling, ""), "Base4::r");
}

#[test]
fn two_operands_that_may_both_be_objects_compare_property_by_property() {
    // Objects of one class compare their properties, recursively through arrays,
    // and any pair may convert an object to a string: a gap whatever the classes.
    let same = |body: &str| file("", &format!("function f(Plain $a, Plain $b) {{ {body} }}"));
    gap(&same("return $a == $b;"), "f", TO_STRING);
    gap(&same("return $a < $b;"), "f", TO_STRING);
    gap(&same("switch ($a) { case $b: return 1; } return 0;"), "f", TO_STRING);
    gap(&file("", "function f(Name $a, $b) { return $a == $b; }"), "f", TO_STRING);
    // Against an operand shown to hold no object at any depth, the class answers.
    covered(&same("return $a == 'x';"), "f");
    covered(&same("switch ($a) { case 'x': return 1; } return 0;"), "f");
    let named = file("", "function f(Name $a, string $s) { return $a == $s; }");
    covered(&named, "f");
    assert_eq!(runs(&named, "f").0, ["io.output.buffer"]);
}

#[test]
fn an_interface_the_project_cannot_read_opens_no_chain_but_a_parent_class_does() {
    // An interface has no body to run on a property access.
    let unread = "class Aware implements \\Psr\\Log\\LoggerAwareInterface { public $logger;\n\
        public function r() { return $this->logger; } }\n\
        interface Wide extends \\Psr\\Log\\LoggerInterface {}\n\
        class Wider implements Wide { public $logger; public function r() { return $this->logger; } }";
    covered(&file(unread, ""), "Aware::r");
    covered(&file(unread, ""), "Wider::r");
    // A parent class it cannot read may declare anything.
    let parent = "class Child extends \\Psr\\Log\\AbstractLogger { public $logger;\n\
        public function r() { return $this->logger; } }";
    gap(&file(parent, ""), "Child::r", PROPERTY);
}

#[test]
fn an_undeclared_name_on_a_bound_class_reaches_the_magic_methods() {
    gap(&file(PROPERTY_CLASSES, ""), "Base::u", PROPERTY);
    gap(&file(PROPERTY_CLASSES, "function f(Shape $o) { return $o->p; }"), "f", PROPERTY);
    gap(&file(PROPERTY_CLASSES, "function f($o) { return $o->p; }"), "f", PROPERTY);
    gap(&file(PROPERTY_CLASSES, "function f(Base $o) { return $o->nope; }"), "f", PROPERTY);
}

#[test]
fn a_declared_property_outside_its_scope_is_not_visible() {
    // `Base::$z` is private to `Base`: from a function it is the magic methods'.
    gap(&file(PROPERTY_CLASSES, "function f(Base $o) { return $o->z; }"), "f", PROPERTY);
    gap(&file(PROPERTY_CLASSES, "function f(Base $o) { return $o->y; }"), "f", PROPERTY);
    covered(&file(PROPERTY_CLASSES, "function f(Base $o) { return $o->x; }"), "f");
}

#[test]
fn an_exact_class_runs_the_magic_methods_it_declares_and_nothing_else() {
    covered(&file(PROPERTY_CLASSES, ""), "Sealed::r");
    covered(&file(PROPERTY_CLASSES, ""), "Sealed::u");
    // The write is the effect lane's state construct; the fetches are covered.
    assert!(operator_gaps(&file(PROPERTY_CLASSES, ""), "Sealed::w").is_empty());
    let read = file(PROPERTY_CLASSES, "function f(Lazy $o) { return $o->a; }");
    covered(&read, "f");
    assert_eq!(runs(&read, "f").0, ["io.output.buffer"]);
    // A store through a variable makes it a written name, which names no class
    // here (the lowering's receivers); `$this` does.
    let stored = "final class Own { public function __set($n, $v) { throw new \\LogicException('s'); }\n\
        public function w() { $this->a = 1; } }";
    let write = file(stored, "");
    assert!(operator_gaps(&write, "Own::w").is_empty());
    assert_eq!(runs(&write, "Own::w").1, ["LogicException"]);
    // A read does not run `__set`.
    assert!(runs(&read, "f").1.is_empty());
}

#[test]
fn a_hooked_property_is_a_gap_because_its_body_is_not_a_site() {
    gap(&file(PROPERTY_CLASSES, ""), "Hooked::r", PROPERTY);
    gap(&file(PROPERTY_CLASSES, ""), "HookedFinal::r", PROPERTY);
}

#[test]
fn a_property_of_a_value_shown_not_to_be_an_object_runs_nothing() {
    // An element of an array may be an object.
    gap(&file(PROPERTY_CLASSES, "function f(array $a) { return $a['k']->p ?? 1; }"), "f", PROPERTY);
    covered(&file(PROPERTY_CLASSES, "function f(int $n) { return $n->p ?? 1; }"), "f");
}

// ---- ArrayAccess ------------------------------------------------------------

const OFFSET_CLASSES: &str = "\
    final class Bag implements \\ArrayAccess {\n\
        public function offsetExists(mixed $o): bool { return true; }\n\
        public function offsetGet(mixed $o): mixed { echo 'g'; return 1; }\n\
        public function offsetSet(mixed $o, mixed $v): void { throw new \\LogicException('s'); }\n\
        public function offsetUnset(mixed $o): void {} }\n\
    class OpenBag implements \\ArrayAccess {\n\
        public function offsetExists(mixed $o): bool { return true; }\n\
        public function offsetGet(mixed $o): mixed { return 1; }\n\
        public function offsetSet(mixed $o, mixed $v): void {}\n\
        public function offsetUnset(mixed $o): void {} }\n";

#[test]
fn an_offset_access_of_a_value_shown_not_to_be_an_object_runs_nothing() {
    covered(&file(OFFSET_CLASSES, "function f(array $a) { return $a['k'] ?? $a[0]; }"), "f");
    covered(&file(OFFSET_CLASSES, "function f(array $a) { $a['k'] = 1; unset($a[0]); return isset($a['k']); }"), "f");
    covered(&file(OFFSET_CLASSES, "function f(string $s) { return $s[0]; }"), "f");
}

#[test]
fn an_exact_array_access_class_runs_the_offset_method_of_the_role() {
    let read = file(OFFSET_CLASSES, "function f(Bag $b) { return $b['k']; }");
    covered(&read, "f");
    assert_eq!(runs(&read, "f"), (vec!["io.output.buffer".to_owned()], Vec::new()));
    // A write through a variable or property makes it a written name, which names
    // no class here; `$this` does, so the write roles are read on `$this`.
    let this_bag = "final class SelfBag implements \\ArrayAccess {\n\
        public function offsetExists(mixed $o): bool { return true; }\n\
        public function offsetGet(mixed $o): mixed { return 1; }\n\
        public function offsetSet(mixed $o, mixed $v): void { throw new \\LogicException('s'); }\n\
        public function offsetUnset(mixed $o): void { echo 'u'; }\n\
        public function set() { $this['k'] = 1; }\n\
        public function append() { $this[] = 1; }\n\
        public function remove() { unset($this['k']); } }";
    let write = file(this_bag, "");
    assert!(operator_gaps(&write, "SelfBag::set").is_empty());
    assert_eq!(runs(&write, "SelfBag::set"), (Vec::new(), vec!["LogicException".to_owned()]));
    assert_eq!(runs(&write, "SelfBag::append").1, ["LogicException"]);
    assert!(operator_gaps(&write, "SelfBag::remove").is_empty());
    assert_eq!(runs(&write, "SelfBag::remove").0, ["io.output.buffer"]);
    // `isset` asks `offsetExists` and then `offsetGet`.
    let tested = file(OFFSET_CLASSES, "function f(Bag $b) { return isset($b['k']); }");
    covered(&tested, "f");
    assert_eq!(runs(&tested, "f").0, ["io.output.buffer"]);
}

#[test]
fn an_exact_class_that_is_not_array_access_runs_nothing() {
    covered(&file(OFFSET_CLASSES, "function f(Plain $p) { return $p['k']; }"), "f");
}

#[test]
fn a_bound_or_unknown_offset_operand_is_a_gap() {
    gap(&file(OFFSET_CLASSES, "function f(OpenBag $b) { return $b['k']; }"), "f", OFFSET);
    gap(&file(OFFSET_CLASSES, "function f(\\ArrayAccess $b) { return $b['k']; }"), "f", OFFSET);
    gap(&file(OFFSET_CLASSES, "function f($b) { return $b['k']; }"), "f", OFFSET);
    gap(&file(OFFSET_CLASSES, "function f($b) { [$x] = $b; return $x; }"), "f", OFFSET);
    // A subclass may implement the interface a non-final class does not.
    gap(&file(OFFSET_CLASSES, "class Pl2 {} function f(Pl2 $b) { return $b['k']; }"), "f", OFFSET);
}

// ---- Iterate ----------------------------------------------------------------

const ITERATE_CLASSES: &str = "\
    final class Counter implements \\Iterator {\n\
        public function rewind(): void { echo 'r'; }\n\
        public function valid(): bool { return false; }\n\
        public function current(): mixed { throw new \\LogicException('c'); }\n\
        public function key(): mixed { return 0; }\n\
        public function next(): void {} }\n\
    final class Coll implements \\IteratorAggregate {\n\
        public function getIterator(): \\Generator { echo 'g'; yield 1; } }\n\
    final class Coll2 implements \\IteratorAggregate {\n\
        public function getIterator(): \\Traversable { return new \\ArrayIterator([]); } }\n\
    final class Coll3 implements \\IteratorAggregate {\n\
        public function getIterator(): Counter { return new Counter(); } }\n";

#[test]
fn iterating_a_value_shown_not_to_be_an_object_runs_nothing() {
    covered(&file(ITERATE_CLASSES, "function f(array $a) { foreach ($a as $k => $v) {} }"), "f");
    covered(&file(ITERATE_CLASSES, "function f() { foreach ([1, 2] as $v) {} return [...[1, 2]]; }"), "f");
}

#[test]
fn an_exact_class_that_is_not_traversable_iterates_its_properties() {
    covered(&file(ITERATE_CLASSES, "function f(Plain $o) { foreach ($o as $k => $v) {} }"), "f");
}

#[test]
fn an_exact_iterator_runs_its_five_methods() {
    let src = file(ITERATE_CLASSES, "function f(Counter $c) { foreach ($c as $v) {} }");
    covered(&src, "f");
    assert_eq!(runs(&src, "f"), (vec!["io.output.buffer".to_owned()], vec!["LogicException".to_owned()]));
    let delegated = file(ITERATE_CLASSES, "function f(Counter $c) { yield from $c; }");
    covered(&delegated, "f");
}

#[test]
fn an_exact_aggregate_runs_get_iterator_and_the_iterator_it_is_shown_to_return() {
    let generator = file(ITERATE_CLASSES, "function f(Coll $c) { foreach ($c as $v) {} }");
    covered(&generator, "f");
    assert_eq!(runs(&generator, "f").0, ["io.output.buffer"]);
    // A returned class the rule can place is followed: its iterator methods run.
    let nested = file(ITERATE_CLASSES, "function f(Coll3 $c) { foreach ($c as $v) {} }");
    covered(&nested, "f");
    assert_eq!(runs(&nested, "f").1, ["LogicException"]);
    // `Traversable` names no class to place.
    gap(&file(ITERATE_CLASSES, "function f(Coll2 $c) { foreach ($c as $v) {} }"), "f", ITERATION);
}

#[test]
fn a_bound_or_unknown_iteration_operand_is_a_gap() {
    gap(&file(ITERATE_CLASSES, "function f(iterable $i) { foreach ($i as $v) {} }"), "f", ITERATION);
    gap(&file(ITERATE_CLASSES, "function f(\\Traversable $i) { foreach ($i as $v) {} }"), "f", ITERATION);
    gap(&file(ITERATE_CLASSES, "function f($i) { foreach ($i as $v) {} }"), "f", ITERATION);
    gap(&file(ITERATE_CLASSES, "function f($i) { return [...$i]; }"), "f", ITERATION);
    gap(&file(ITERATE_CLASSES, "class Pl2 {} function f(Pl2 $i) { foreach ($i as $v) {} }"), "f", ITERATION);
}

// ---- Clone ------------------------------------------------------------------

const CLONE_CLASSES: &str = "\
    final class Dup { public function __clone() { echo 'c'; } }\n\
    final class Dup2 { public function __clone() { throw new \\LogicException('c'); }\n\
        public function __set($n, $v) { echo 's'; } }\n\
    class Plain2 {}\n";

#[test]
fn cloning_an_exact_class_runs_its_clone_method_when_it_has_one() {
    covered(&file(CLONE_CLASSES, "function f(Plain $o) { return clone $o; }"), "f");
    let src = file(CLONE_CLASSES, "function f(Dup $o) { return clone $o; }");
    covered(&src, "f");
    assert_eq!(runs(&src, "f").0, ["io.output.buffer"]);
    let thrown = file(CLONE_CLASSES, "function f(Dup2 $o) { return clone $o; }");
    assert_eq!(runs(&thrown, "f").1, ["LogicException"]);
}

#[test]
fn clone_with_a_property_list_also_runs_set() {
    // The parser reads `clone(...)` as a call of a function named `clone`, which
    // the builtin rules gap on their own; only the operator's kinds are read.
    // A variable passed to that call may be taken by reference, so it names no
    // class (the lowering says so); `$this` and `new` do.
    let with_set = "final class K { public function __clone() { throw new \\LogicException('c'); }\n\
        public function __set($n, $v) { echo 's'; }\n\
        public function m() { return clone($this, ['a' => 1]); } }";
    let src = file(with_set, "");
    assert!(operator_gaps(&src, "K::m").is_empty());
    assert_eq!(
        runs(&src, "K::m"),
        (vec!["io.output.buffer".to_owned()], vec!["LogicException".to_owned()])
    );
    let plain = "final class P { public function m() { return clone($this, ['a' => 1]); } }";
    assert!(operator_gaps(&file(plain, ""), "P::m").is_empty());
    let open = "class P { public function m() { return clone($this, ['a' => 1]); } }";
    assert_eq!(operator_gaps(&file(open, ""), "P::m"), [CLONE]);
    let fresh = file("", "function f() { return clone(new Plain(), ['a' => 1]); }");
    assert!(operator_gaps(&fresh, "f").is_empty());
}

#[test]
fn cloning_a_bound_or_unknown_operand_is_a_gap() {
    gap(&file(CLONE_CLASSES, "function f(Plain2 $o) { return clone $o; }"), "f", CLONE);
    gap(&file(CLONE_CLASSES, "function f(Shape $o) { return clone $o; }"), "f", CLONE);
    gap(&file(CLONE_CLASSES, "function f($o) { return clone $o; }"), "f", CLONE);
    let this = "class K { public function with() { return clone $this; } }";
    gap(&file(this, ""), "K::with", CLONE);
    let sealed = "final class K { public function with() { return clone $this; } }";
    covered(&file(sealed, ""), "K::with");
    let fixed = "class K { final public function __clone() { echo 'k'; } public function with() { return clone $this; } }";
    let src = file(fixed, "");
    covered(&src, "K::with");
    assert_eq!(runs(&src, "K::with").0, ["io.output.buffer"]);
}

// ---- __call -----------------------------------------------------------------

#[test]
fn a_missing_method_on_an_exact_class_that_declares_call_is_an_edge() {
    let magic = "final class Magic { public function __call($n, $a) { echo 'c'; }\n\
                 public static function __callStatic($n, $a) { throw new \\LogicException('s'); } }";
    let instance = file(magic, "function f() { return (new Magic())->nope(); }");
    covered(&instance, "f");
    assert_eq!(runs(&instance, "f").0, ["io.output.buffer"]);
    let statically = file(magic, "function f() { return Magic::nope(); }");
    covered(&statically, "f");
    assert_eq!(runs(&statically, "f").1, ["LogicException"]);
    // A class declaring neither keeps the gap.
    gap(&file("", "function f() { return (new Plain())->nope(); }"), "f", "method-not-found");
}

// ---- both lanes, and what is not a site -------------------------------------

#[test]
fn a_call_inside_an_instance_runs_this_objects_own_call() {
    // `parent::missing()` in an instance method runs `$this`'s `__call`, which a
    // subclass may override: the named class's is not the answer.
    let open = "class Foo { public function __call($n, $a) { echo 'f'; } }\n\
        class Bar extends Foo { public function viaParent() { return parent::missing(); }\n\
            public function viaName() { return Foo::missing(); } }";
    gap(&file(open, ""), "Bar::viaParent", "method-not-found");
    gap(&file(open, ""), "Bar::viaName", "method-not-found");
    // Pinned: the enclosing class is final, or its `__call` is.
    let pinned = "class Foo { public function __call($n, $a) { echo 'f'; } }\n\
        final class Bar extends Foo { public function viaParent() { return parent::missing(); } }\n\
        class Baz extends Foo { final public function __call($n, $a) { echo 'z'; }\n\
            public function viaParent() { return parent::missing(); } }";
    covered(&file(pinned, ""), "Bar::viaParent");
    covered(&file(pinned, ""), "Baz::viaParent");
    assert_eq!(runs(&file(pinned, ""), "Baz::viaParent").0, ["io.output.buffer"]);
    // A frame that cannot be an instance of the named class runs the named class's.
    let unrelated = "class Foo { public function __call($n, $a) { echo 'f'; } }\n\
        class Other { public function m() { return Foo::missing(); } }";
    covered(&file(unrelated, ""), "Other::m");
}

#[test]
fn a_variable_a_call_may_fill_by_reference_is_not_object_free() {
    let setv = "function setv(&$x) { $x = new Name(); }\n";
    gap(&file(setv, "function f() { setv($v); echo $v; }"), "f", TO_STRING);
    gap(&file(setv, "function f() { setv($v); return $v == 'x'; }"), "f", TO_STRING);
    gap(&file(setv, "function f() { setv($a); foreach ($a as $x) {} }"), "f", ITERATION);
    // Nothing takes it by reference: it holds the scalar it was given.
    covered(&file("", "function f() { $v = 'x'; echo $v; }"), "f");
}

#[test]
fn a_coalescing_assignment_asks_isset_of_its_intermediates() {
    let src = "final class K { public function __isset($n) { throw new \\LogicException('i'); }\n\
        public function __get($n) { return 1; }\n\
        public function g() { $this->p->q ??= 1; } }";
    assert_eq!(runs(&file(src, ""), "K::g").1, ["LogicException"]);
}

#[test]
fn an_array_iterator_may_be_subclassed_and_array_object_routes_properties_to_offsets() {
    let src = "final class Coll4 implements \\IteratorAggregate {\n\
        public function getIterator(): \\ArrayIterator { return new \\ArrayIterator([]); } }\n\
        final class Sup extends \\ArrayObject { public $p; public function r() { return $this->p; } }\n\
        final class Ex extends \\RuntimeException { public $p; public function r() { return $this->p; } }";
    gap(&file(src, "function f(Coll4 $c) { foreach ($c as $v) {} }"), "f", ITERATION);
    gap(&file(src, ""), "Sup::r", PROPERTY);
    covered(&file(src, ""), "Ex::r");
}

#[test]
fn the_unknown_operand_gap_survives_through_a_caller() {
    let src = file("", "function g($o) { return 'a' . $o; }\nfunction f($o) { return g($o); }");
    assert_eq!(gaps(&src, "g"), [TO_STRING]);
    assert_eq!(gaps(&src, "f"), [TO_STRING]);
}

// ---- a call result is read off its declared return (S8, issue #877) ----------

/// The classes and helpers the S8 rows name: `S` converts to a string with an effect;
/// a project function whose native return is a string, one with none, one returning
/// `S`; a final and a non-final class with a `string`-returning method.
const RETURNS: &str = "class S { public function __toString(): string { echo 's'; return '8'; } }\n\
    function helper_string(string $x): string { return $x . '!'; }\n\
    function helper_untyped($x) { return $x; }\n\
    function helper_object(): S { return new S; }\n\
    final class Acc { public function name(): string { return 'n'; } }\n\
    class OpenAcc { public function name(): string { return 'n'; } }\n";

fn returns(function: &str) -> String {
    file(RETURNS, function)
}

/// The effect lane's and the throw lane's gap kinds, which differ where only one
/// of them has a row to miss.
fn lane_gaps(src: &str, symbol: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let s = summary(src, symbol);
    (s.gaps, s.throws_gaps)
}

/// Rows 8.1, 8.2, 8.5: a builtin's mined return holds no object, so the string
/// conversion of its result runs nothing. The classifier reads the phpdoc-shaped
/// spelling: `int<1, max>|0`, `uppercase-string`, `non-empty-string|false`.
#[test]
fn s8_a_builtins_scalar_return_runs_nothing_at_a_string_conversion() {
    covered(&returns("function f(string $s) { return 'x' . strlen($s); }"), "f");
    covered(&returns("function f(string $s) { return 'x' . strtoupper($s); }"), "f");
    covered(&returns("function f() { return 'x' . json_encode(['a' => 1]); }"), "f");
    covered(&returns("function f(string $s) { return \"v: {$s}\" . trim($s) . intdiv(3, 2); }"), "f");
    // A comparison reads an array's elements, so a scalar result is enough and an
    // array one is not: `array_keys` is `list<int|string>`, which holds no object.
    covered(&returns("function f(array $a) { return array_keys($a) == 'x'; }"), "f");
    gap(&returns("function f(array $a) { return array_values($a) == 'x'; }"), "f", TO_STRING);
}

/// Row 8.13 and the rows a builtin's declared return does not rule out: `mixed`, a
/// class (`GMP`, row 8.11) and an unmodeled spelling stay a gap.
#[test]
fn s8_a_builtins_object_or_mixed_return_is_still_unknown() {
    for call in ["current($a)", "json_decode('1')", "date_create()", "gmp_init(5)"] {
        let src = returns(&format!("function f(array $a) {{ return 'x' . {call}; }}"));
        assert_eq!(operator_gaps(&src, "f"), [TO_STRING], "{call}");
    }
    let gmp = lane_gaps(&returns("function f() { return 'n=' . gmp_init(5); }"), "f");
    assert_eq!(gmp, (vec!["no-effect-row", TO_STRING], vec!["no-throw-row", TO_STRING]));
}

/// Rows 8.6, 8.7 and 8.10: a project function's or an exact method's native return.
/// A hint that admits a class, or no hint, proves nothing.
#[test]
fn s8_a_project_functions_native_return_decides() {
    covered(&returns("function f(string $s) { return 'x' . helper_string($s); }"), "f");
    covered(&returns("function f() { return 'x' . (new Acc())->name(); }"), "f");
    gap(&returns("function f($x) { return 'x' . helper_untyped($x); }"), "f", TO_STRING);
    gap(&returns("function f() { return 'x' . helper_object(); }"), "f", TO_STRING);
    let hints = "function r_opt(): ?string { return null; }\n\
        function r_arr(): array { return []; }\n\
        function r_union(): int|string|null { return 1; }\n\
        function r_iter(): iterable { return []; }\n\
        function r_mixed(): mixed { return 1; }\n\
        function r_void(): void {}\n\
        function r_iface(): \\Stringable { return new S(); }";
    for call in ["r_opt()", "r_arr()", "r_union()"] {
        let src = file(&format!("{RETURNS}{hints}"), &format!("function f() {{ return 'x' . {call}; }}"));
        covered(&src, "f");
    }
    for call in ["r_iter()", "r_mixed()", "r_void()", "r_iface()"] {
        let src = file(&format!("{RETURNS}{hints}"), &format!("function f() {{ return 'x' . {call}; }}"));
        gap(&src, "f", TO_STRING);
    }
}

/// A namespaced function that shadows a builtin is the one PHP calls, and a
/// conditional declaration binds by load order: neither is the builtin's row.
#[test]
fn s8_a_call_php_may_bind_elsewhere_proves_nothing() {
    let shadowed = "<?php\nnamespace App;\nclass S { public function __toString(): string { echo 's'; return '8'; } }\n\
        function strlen(string $s): S { return new S(); }\n\
        function f(string $s) { return 'x' . strlen($s); }\n";
    gap(shadowed, "f", TO_STRING);
    let conditional = "<?php\nclass S { public function __toString(): string { echo 's'; return '8'; } }\n\
        if (!function_exists('cond')) { function cond(): string { return 's'; } }\n\
        function f() { return 'x' . cond(); }\n";
    gap(conditional, "f", TO_STRING);
}

/// Row 8.8: a final engine accessor holds a string, in both lanes; the throw lane's
/// `declared-receiver` (the interface envelope it cannot read) stays.
#[test]
fn s8_a_final_engine_accessor_returns_a_string() {
    let src = returns("function f(\\Throwable $e) { return 'x' . $e->getMessage(); }");
    assert_eq!(lane_gaps(&src, "f"), (vec![], vec!["declared-receiver"]));
    let exact = returns("function f() { return 'x' . (new \\RuntimeException('m'))->getMessage(); }");
    assert!(operator_gaps(&exact, "f").is_empty());
    // `getTrace()` is an array that may hold objects: not an object itself.
    let trace = returns("function f(\\Throwable $e) { return 'x' . $e->getTrace(); }");
    assert!(!lane_gaps(&trace, "f").0.contains(&TO_STRING));
    // Any other engine method on a bound receiver may be a userland override of a
    // tentative return type: `Countable::count()` returns whatever the class does.
    let count = returns("function f(\\Countable $c) { return 'x' . $c->count(); }");
    assert!(lane_gaps(&count, "f").0.contains(&TO_STRING));
    // An exact engine receiver runs the engine's own method.
    let iterator = returns("function f() { return 'x' . (new \\ArrayIterator([1]))->count(); }");
    assert!(operator_gaps(&iterator, "f").is_empty());
}

/// Row 8.15: a bound receiver's method with a native return holds that type under
/// every subclass, since PHP refuses a non-covariant override; the call's own gap
/// (`declared-receiver`, `non-final-this`) stays, and only the operator's goes.
#[test]
fn s8_a_bound_receivers_native_return_binds_every_subclass() {
    let src = returns("function f(OpenAcc $a) { return 'x' . $a->name(); }");
    assert!(operator_gaps(&src, "f").is_empty());
    assert_eq!(lane_gaps(&src, "f").1, ["declared-receiver"]);
    let this = "class K { public function name(): string { return 'k'; }\n\
        public function m() { return 'x' . $this->name(); } }";
    assert!(operator_gaps(&returns(this), "K::m").is_empty());
    let untyped = "class K { public function name() { return 'k'; }\n\
        public function m() { return 'x' . $this->name(); } }";
    assert_eq!(operator_gaps(&returns(untyped), "K::m"), [TO_STRING]);
    let object = "class K { public function me(): static { return $this; }\n\
        public function m() { return 'x' . $this->me(); } }";
    assert_eq!(operator_gaps(&returns(object), "K::m"), [TO_STRING]);
    let iface = "interface HasName { public function name(): string; }";
    let src = file(&format!("{RETURNS}{iface}"), "function f(HasName $h) { return 'x' . $h->name(); }");
    assert!(operator_gaps(&src, "f").is_empty());
}

/// A method the enclosing scope cannot reach is `__call`'s, which may return an
/// object whatever the hidden method's hint says.
#[test]
fn s8_a_method_the_scope_cannot_reach_proves_nothing() {
    let hidden = "class Priv { private function secret(): string { return 's'; }\n\
        public function __call($n, $a) { return new S(); } }\n\
        function outside(Priv $p) { return 'x' . $p->secret(); }";
    assert!(lane_gaps(&returns(hidden), "outside").0.contains(&TO_STRING));
    // Inside the class the private method is the one called.
    let inside = "class Priv { private function secret(): string { return 's'; }\n\
        public function m() { return 'x' . $this->secret(); } }";
    covered(&returns(inside), "Priv::m");
}

/// An array result is no object: iterating it and reading an offset of it run nothing
/// (neighbouring rows, the two largest classes the public corpora move). A result that
/// may be an object, or holds no type, keeps the gap.
#[test]
fn s8_iterating_or_indexing_an_array_result_runs_nothing() {
    let covered_ops = |function: &str, symbol: &str| {
        let found = operator_gaps(&returns(function), symbol);
        assert!(found.is_empty(), "{function}: {found:?}");
    };
    covered_ops("function f(string $s) { foreach (explode(',', $s) as $p) {} }", "f");
    covered_ops("function f(string $s) { return explode(',', $s)[0]; }", "f");
    covered_ops("function f(array $a) { foreach (array_map('trim', $a) as $k => $v) {} }", "f");
    covered_ops(
        "class K { private function parts(): array { return []; }\n\
         public function f() { foreach ($this->parts() as $p) {} return $this->parts()[0]; } }",
        "K::f",
    );
    let iterate = |call: &str| {
        operator_gaps(&returns(&format!("function f($x, array $a) {{ foreach ({call} as $p) {{}} }}")), "f")
    };
    assert_eq!(iterate("current($a)"), [ITERATION]);
    assert_eq!(iterate("helper_untyped($x)"), [ITERATION]);
    assert_eq!(iterate("new_iterator()"), [ITERATION]);
}

/// Rows 8.9 and 8.12: an untyped operand and arithmetic stay a gap (the arithmetic
/// sub-slice of #877 is deferred: `GMP` and `BcMath\Number` overload it).
#[test]
fn s8_an_untyped_operand_and_arithmetic_stay_a_gap() {
    gap(&returns("function f($x) { return 'x' . $x; }"), "f", TO_STRING);
    gap(&returns("function f(int $n) { return 'n=' . ($n * 2); }"), "f", TO_STRING);
    // A local assigned from a call is not read either: the syntax pass cannot ask
    // the catalog what the call returns.
    gap(&returns("function f(string $s) { $t = trim($s); return 'x' . $t; }"), "f", TO_STRING);
}
