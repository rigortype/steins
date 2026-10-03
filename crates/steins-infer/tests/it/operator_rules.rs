//! The operator sites resolve against the operand's class (ADR-0099 §4.3, §4.4,
//! issue #859): converting an object to a string, fetching or storing a property
//! on it, indexing it, iterating it and cloning it run the user method the class
//! names, and the site is a coverage gap where no class can be pinned. Each
//! family is read for an operand shown not to be an object, an exact class with
//! the method and without, a bound (a non-final `$this`, an interface, a
//! non-final class), and an operand nothing is known of. Both lanes read the one
//! resolution, so every case asserts the two agree.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{EffectSummary, effect_summaries_project, effect_summary};
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

/// A conditionally declared class anywhere on the chain from the receiver's up to the declaring
/// one binds by load order, so the declaration read at the top may not be the one below it
/// (the dispatch-side twin is issue #998).
#[test]
fn s8_a_conditional_class_in_the_middle_of_the_chain_proves_nothing() {
    let chain = |open: &str, close: &str| {
        let classes = format!(
            "class Base2 {{ public function name(): string {{ return 'b'; }} }}\n\
             {open}class Mid2 extends Base2 {{}}{close}\nclass Leaf2 extends Mid2 {{}}"
        );
        file(&format!("{RETURNS}{classes}"), "function f(Leaf2 $l) { return 'x' . $l->name(); }")
    };
    assert!(operator_gaps(&chain("", ""), "f").is_empty());
    let guarded = chain("if (!class_exists('Mid2')) { ", " }");
    assert_eq!(operator_gaps(&guarded, "f"), [TO_STRING]);
}

/// Row 8.8, as the review of #996 corrected it: a final `Throwable` accessor holds what its
/// typed property backs. `getFile()`, `getLine()` and `getTraceAsString()` hold no object, in
/// both lanes (the throw lane keeps the `declared-receiver` of the interface envelope it
/// cannot read). `getMessage()` and `getCode()` read an untyped property a subclass may fill
/// with an object (`getMessage()` then runs its `__toString`, witnessed on PHP 8.5.11, and
/// `getCode()` returns it), so they stay a gap on every receiver (issue #997 is the same
/// premise on master, in the effect lane's own accessor row).
#[test]
fn s8_a_final_engine_accessor_returns_a_string() {
    for accessor in ["getTraceAsString()", "getTrace()"] {
        let src = returns(&format!("function f(\\Throwable $e) {{ return 'x' . $e->{accessor}; }}"));
        assert_eq!(lane_gaps(&src, "f"), (vec![], vec!["declared-receiver"]), "{accessor}");
    }
    // `getFile()` and `getLine()` read a typed property through `__get` once a subclass
    // `unset`s it (the second review of #996, witnessed on PHP 8.5.11: `__get` runs and its
    // object's `__toString` with it), so a bound receiver is not read; an exact class, or a
    // final one whose chain declares no `__get`, is.
    for accessor in ["getFile()", "getLine()"] {
        let bound = returns(&format!("function f(\\Throwable $e) {{ return 'x' . $e->{accessor}; }}"));
        assert_eq!(operator_gaps(&bound, "f"), [TO_STRING], "{accessor}");
        let open = file(
            &format!("{RETURNS}class RtOpen extends \\RuntimeException {{}}"),
            &format!("function f(RtOpen $e) {{ return 'x' . $e->{accessor}; }}"),
        );
        assert_eq!(operator_gaps(&open, "f"), [TO_STRING], "{accessor}");
        let magic = format!(
            "final class RtMagic extends \\RuntimeException {{\n\
             public function __get($n) {{ return new S(); }}\n\
             public function m() {{ return 'x' . $this->{accessor}; }} }}"
        );
        assert_eq!(operator_gaps(&returns(&magic), "RtMagic::m"), [TO_STRING], "{accessor}");
        let sealed = format!(
            "final class RtFinal extends \\RuntimeException {{\n\
             public function m() {{ return 'x' . $this->{accessor}; }} }}"
        );
        assert!(operator_gaps(&returns(&sealed), "RtFinal::m").is_empty(), "{accessor}");
        let new = format!("function f() {{ return 'x' . (new \\RuntimeException('m'))->{accessor}; }}");
        assert!(operator_gaps(&returns(&new), "f").is_empty(), "{accessor}");
        // `parent::` runs on `$this`, which a subclass of the enclosing class may be.
        let parent = format!(
            "class RtBase extends \\RuntimeException {{\n\
             public function m() {{ return 'x' . parent::{accessor}; }} }}"
        );
        assert_eq!(operator_gaps(&returns(&parent), "RtBase::m"), [TO_STRING], "{accessor}");
    }
    for accessor in ["getMessage()", "getCode()"] {
        for receiver in ["\\Throwable $e", "\\RuntimeException $e", "RtEvil $e"] {
            let function = format!("function f({receiver}) {{ return 'x' . $e->{accessor}; }}");
            let src = file(&format!("{RETURNS}class RtEvil extends \\RuntimeException {{}}"), &function);
            assert_eq!(operator_gaps(&src, "f"), [TO_STRING], "{accessor} on {receiver}");
        }
        let new = format!("function f() {{ return 'x' . (new \\RuntimeException('m'))->{accessor}; }}");
        assert_eq!(operator_gaps(&returns(&new), "f"), [TO_STRING], "{accessor}");
    }
    // The review's witness: a constructor that hands a subclass's message on is not pure.
    let relay = "class RT extends \\RuntimeException {}\nclass PFE extends RT {}\n\
        final class Relay extends RT { public function __construct(PFE $e) {\n\
        parent::__construct($e->getMessage(), $e->getCode()); } }";
    let s = summary(&returns(relay), "Relay::__construct");
    assert!(!s.exhaustive, "{s:?}");
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

// ---- Drop sites (ADR-0100 §7, issue #882) -------------------------------------
//
// The witness rows of the S6 revision (#915), each run on PHP 8.5: a dropped value
// may run `__destruct`, so every drop of a value whose class reaches a destructor is
// a `destructor` gap in both lanes, and no drop is an edge. The rows the table marks
// must-stay read exactly as they did before the sites landed.

const DESTRUCTOR: &str = "destructor";

/// The classes the rows share. `D` declares the destructor everything else reaches or
/// does not reach; every other class is named for what it holds.
const DROP_CLASSES: &str = "<?php\n\
    class D { public function __destruct() { echo '[D::__destruct]'; } }\n\
    final class FD { public function __destruct() { echo '[FD::__destruct]'; } }\n\
    final class ND { }\n\
    final class Empty_ { }\n\
    class Base { }\n\
    final class SubBase extends Base { public function __destruct() { echo '[SubBase]'; } }\n\
    final class HoldsTyped { private D $d; public function __construct() { $this->d = new D; } }\n\
    final class HoldsHolder { private HoldsTyped $h; }\n\
    final class HoldsNullable { private ?D $d = null; }\n\
    final class HoldsUntyped { private $d; public function __construct() { $this->d = new D; } }\n\
    final class HoldsScalar { private int $n = 1; private string $s = 'x'; private ?float $f = null; }\n\
    final class HoldsArray { private array $a = []; public function __construct() { $this->a[] = new D; } }\n\
    final class HoldsMixed { private mixed $m; private object $o; private iterable $i; private \\Closure $c; }\n\
    class Holder { }\n\
    final class SubHolder extends Holder { private D $d; public function __construct() { $this->d = new D; } }\n\
    class VD { public function __destruct() { echo '[VD]'; } }\n\
    class R { public function m() { echo '<m>'; } public function __destruct() { echo '[R]'; } }\n\
    class M { public function __call($n, $a) { echo '[__call]'; } }\n\
    trait T { public function bye() { echo '[bye]'; } }\n\
    class U { use T { bye as __destruct; } }\n\
    trait T2 { public function __destruct() { echo '[T2]'; } }\n\
    trait T1 { use T2; }\n\
    class UT { use T1; }\n\
    interface I { public function __destruct(); }\n\
    class CI implements I { public function __destruct() { echo '[CI]'; } }\n\
    function foo(D $d) { echo '<foo-body>'; }\n\
    function bar($d) { echo '<bar-body>'; }\n";

fn drops_in(function: &str) -> String {
    format!("{DROP_CLASSES}{function}\n")
}

/// Whether `symbol` carries the destructor gap, which both lanes must agree on.
fn dtor(src: &str, symbol: &str) -> bool {
    let s = summary(src, symbol);
    assert_eq!(
        s.gaps.contains(&DESTRUCTOR),
        s.throws_gaps.contains(&DESTRUCTOR),
        "the two lanes read one resolution: {s:?}\n{src}"
    );
    assert_eq!(s.exhaustive, s.gaps.is_empty(), "{s:?}");
    s.gaps.contains(&DESTRUCTOR)
}

fn dtor_gap(function: &str, symbol: &str) {
    assert!(dtor(&drops_in(function), symbol), "{symbol} should be a destructor gap\n{function}");
}

fn dtor_none(function: &str, symbol: &str) {
    assert!(!dtor(&drops_in(function), symbol), "{symbol} runs no destructor\n{function}");
}

/// Rows 6.1 to 6.3: a body-local `new D` dropped by `unset`, a reassignment, or the end
/// of the scope. Row 6.14: the same through a final class.
#[test]
fn s6_a_local_new_whose_class_declares_a_destructor_is_a_gap_at_each_drop() {
    dtor_gap("function f() { $f = new D; unset($f); echo '<after>'; return 1; }", "f");
    dtor_gap("function f() { $f = new D; $f = null; echo '<after>'; return 1; }", "f");
    dtor_gap("function f() { $f = new D; echo '<body>'; return 1; }", "f");
    dtor_gap("function f() { $f = new FD; unset($f); }", "f");
    // The first write alone is a scope-exit site; so is a write of another class.
    dtor_gap("function f() { $f = new ND; $f = new D; }", "f");
}

/// Rows 6.4 to 6.7 and 6.14: a parameter is read by its declared hint, though the frame
/// writes it, and whether the caller still holds it makes no difference (6.5).
#[test]
fn s6_a_parameter_is_a_gap_by_its_hint_whether_or_not_the_frame_writes_it() {
    dtor_gap("function f(D $d) { $d = null; echo '<after>'; return 1; }", "f");
    dtor_gap("function f(D $d) { echo '<body>'; return 1; }", "f");
    // 6.5: the caller keeps a reference, so `d` runs nothing: still a may-run, not an edge.
    dtor_gap("function k() { $keep = new D; foo($keep); echo '<caller>'; unset($keep); }", "k");
    // 6.7: the bound's subclass declares the destructor.
    dtor_gap("function f(Base $b) { $b = null; }", "f");
    dtor_gap("function f(FD $d) { $d = null; }", "f");
    dtor_gap("function f(?D $d) { $d = null; }", "f");
    dtor_gap("function f(D|int $d) { }", "f");
}

/// Row 6.8: no destructor, no property. The residue shapes of 6.9 and 6.13 stay as well.
#[test]
fn s6_a_class_that_reaches_no_destructor_runs_nothing() {
    dtor_none("function f() { $e = new Empty_; unset($e); $e = new Empty_; }", "f");
    dtor_none("function f(Empty_ $e) { $e = null; }", "f");
    dtor_none("function f(ND $n, int $i, string $s, ?float $x, bool $b) { $n = null; }", "f");
    // The over-reporting guard: a parameter that cannot name a destructor class is no gap.
    dtor_none("function f(array $a, mixed $m, object $o, iterable $i, callable $c) { $a = null; }", "f");
    dtor_none("function f(\\Closure $c, \\Generator $g, \\stdClass $s, \\Throwable $t) { $c = null; }", "f");
    dtor_none("function f($x) { unset($x); $x = new D; }", "f");
}

/// Row 6.9: an unknown-class value is recorded residue, not a gap.
#[test]
fn s6_an_untyped_value_is_residue() {
    dtor_none("function f($x) { unset($x); }", "f");
    dtor_none("function f($x) { $x = null; }", "f");
    dtor_none("function f() { $x = make(); unset($x); }", "f");
    dtor_none("function f() { $x = $this->make(); $x = null; }", "f");
}

/// Row 6.13: `array_splice`, and an array whatever it holds, are residue (6.21).
#[test]
fn s6_arrays_are_residue_whatever_they_hold() {
    dtor_none("function f() { $a = [new D]; array_splice($a, 0, 1); }", "f");
    dtor_none("function f() { $a = [new D]; unset($a); }", "f");
    dtor_none("function f(array $a) { $a = null; }", "f");
    dtor_none("function f() { $h = new HoldsArray; unset($h); }", "f");
    dtor_none("function f(HoldsArray $h) { $h = null; }", "f");
}

/// Rows 6.10 and 6.11: a `new` that escapes is no site in this body.
#[test]
fn s6_a_new_that_escapes_is_no_site() {
    dtor_none("function f(): D { return new D; }", "f");
    dtor_none("function f() { $o = new stdClass; $o->d = new D; return $o; }", "f");
    dtor_none("function f() { $x = new stdClass; $x->d = new D; return $x; }", "f");
    dtor_none("function f() { return [new D]; }", "f");
    dtor_none("function f() { $a = [new D, new D]; return count($a); }", "f");
    dtor_none("function f() { yield new D; }", "f");
    dtor_none("function f() { return function () { return new D; }; }", "f");
    // The escaping `new` still reads exactly as it did: row 6.11's other gaps are the same.
    let escapes = drops_in("function f() { $h = new stdClass; $h->d = new D; return $h; }");
    assert_eq!(summary(&escapes, "f").gaps, ["state-construct", "operator-magic-property"]);
}

/// Rows 6.12 and 6.16: a statement-position `new D` is a may-run, since a constructor can
/// keep the object alive past the statement, a self-reference defers it to the collector
/// and a throwing constructor never runs it; an edge would be a false positive.
#[test]
fn s6_a_statement_new_is_a_gap_and_never_an_edge() {
    dtor_gap("function f() { new D; echo '<after>'; return 1; }", "f");
    let stores = "class SD { public static array $all = [];\n\
        public function __construct() { self::$all[] = $this; }\n\
        public function __destruct() { echo '[SD]'; } }\n\
        function f() { new SD; echo '<after-statement>'; return 1; }";
    let src = format!("<?php\n{stores}\n");
    assert!(dtor(&src, "f"));
    // The constructor's own gaps, inherited by the statement, read as they did.
    let gaps = summary(&src, "f").gaps;
    assert!(gaps.contains(&"state-construct") && gaps.contains(&OFFSET), "{gaps:?}");
    let cycle = "class CD { public $self;\n\
        public function __construct() { $this->self = $this; }\n\
        public function __destruct() { echo '[CD]'; } }\n\
        function f() { new CD; echo '<after-statement>'; return 1; }";
    assert!(dtor(&format!("<?php\n{cycle}\n"), "f"));
    let throws = "class TD { public function __construct() { throw new \\RuntimeException('no'); }\n\
        public function __destruct() { echo '[TD]'; } }\n\
        function f() { try { new TD; } catch (\\RuntimeException $e) { echo '<caught>'; } return 1; }";
    assert!(dtor(&format!("<?php\n{throws}\n"), "f"));
}

/// Row 6.17: a temporary handed to a call, or a method called on one, dies in the caller;
/// the callee is judged on its own parameter (`bar`'s is untyped, so none).
#[test]
fn s6_a_new_temporary_in_argument_or_receiver_position_is_a_gap_in_the_caller() {
    dtor_gap("function f() { foo(new D); echo '<after>'; }", "f");
    dtor_gap("function f() { bar(new D); echo '<after>'; }", "f");
    dtor_gap("function f() { (new R)->m(); echo '<after>'; }", "f");
    dtor_gap("function f($o) { $o->put(k: new D()); }", "f");
    dtor_gap("function f() { new ND(new D); }", "f");
    dtor_gap("function f() { return strlen((string) foo(new D)); }", "f");
    dtor_none("function f() { bar(new ND); (new ND)->x(); }", "f");
    dtor_none("function f() { bar(new \\ArrayObject([])); new \\stdClass; }", "f");
    // `bar` itself has no site: an untyped parameter is residue.
    dtor_none("function g($d) { $d = null; }", "g");
}

/// Rows 6.18 and 6.19, and the guard against the universe gate: a typed property that
/// names a class reaching a destructor is the hop; an untyped, `array`, `mixed`,
/// `object`, `iterable` or engine-class property is recorded residue.
#[test]
fn s6_a_typed_property_hops_to_a_class_that_reaches_a_destructor() {
    dtor_gap("function f() { $h = new HoldsTyped; unset($h); }", "f");
    dtor_gap("function f(HoldsTyped $h) { $h = null; }", "f");
    dtor_gap("function f() { $h = new HoldsHolder; unset($h); }", "f");
    dtor_gap("function f(HoldsHolder $h) { }", "f");
    dtor_gap("function f(HoldsNullable $h) { }", "f");
    // 6.19 and 6.20: residue and scalars read as they did.
    dtor_none("function f() { $h = new HoldsUntyped; unset($h); }", "f");
    dtor_none("function f(HoldsUntyped $h) { $h = null; }", "f");
    dtor_none("function f() { $h = new HoldsScalar; unset($h); }", "f");
    dtor_none("function f(HoldsScalar $h) { $h = null; }", "f");
    dtor_none("function f() { $h = new HoldsMixed; unset($h); }", "f");
    dtor_none("function f(HoldsMixed $h) { }", "f");
}

/// Property types that refer to each other end the walk, and an inherited property counts.
#[test]
fn s6_the_property_hop_ends_on_a_cycle_and_reads_the_inherited_chain() {
    let cycle = "class CycA { private CycB $b; }\nclass CycB { private CycA $a; }\n\
        function f(CycA $a) { $a = null; }";
    assert!(!dtor(&drops_in(cycle), "f"));
    let inherited = "class HoldsParent { protected D $d; }\nfinal class Child extends HoldsParent { }\n\
        function f(Child $c) { $c = null; }";
    assert!(dtor(&drops_in(inherited), "f"));
    // A hop through a union property: any member that reaches counts.
    let union = "final class HoldsUnion { private int|D $v = 1; }\nfunction f(HoldsUnion $h) { }";
    assert!(dtor(&drops_in(union), "f"));
}

/// Row 6.22: a subclass's own property is residue when the subject is bound.
#[test]
fn s6_a_subclass_s_own_properties_are_residue_when_the_subject_is_bound() {
    dtor_none("function f(Holder $h) { $h = null; }", "f");
    // The exact class reads its own.
    dtor_gap("function f(SubHolder $h) { $h = null; }", "f");
    dtor_gap("function f() { $h = new SubHolder; unset($h); }", "f");
}

/// Row 6.23: captures and suspended frames are recorded residue.
#[test]
fn s6_closures_generators_and_fibers_are_residue() {
    dtor_none("function f(\\Closure $c) { $c = null; }", "f");
    dtor_none("function f(\\Generator $g) { $g = null; }", "f");
    dtor_none(
        "function f() { $fb = new \\Fiber(function () { try { \\Fiber::suspend(1); } \
         finally { echo '[fiber finally]'; } }); $fb->start(); unset($fb); }",
        "f",
    );
    dtor_none("function f() { $c = (function () { return 1; })(); unset($c); }", "f");
}

/// Row 6.24: an anonymous class is its own body's subject.
#[test]
fn s6_an_anonymous_class_carries_its_destructor_on_the_binding() {
    dtor_gap("function f() { $x = new class { function __destruct() { echo '[anon]'; } }; unset($x); }", "f");
    dtor_gap("function f() { foo(new class { function __destruct() {} }); }", "f");
    // A trait that brings one counts, a clean one does not (S6c, below); a parent that
    // reaches one is the class.
    dtor_gap("function f() { $x = new class { use T2; }; unset($x); }", "f");
    dtor_none("function f() { $x = new class { use T; }; unset($x); }", "f");
    dtor_gap("function f() { $x = new class extends D {}; unset($x); }", "f");
    dtor_gap("function f() { $x = new class extends HoldsTyped {}; unset($x); }", "f");
    dtor_none("function f() { $x = new class { public int $n = 1; }; unset($x); }", "f");
    // A parent no file declares records an unknown-class gap at the `new class` (S6c, below).
    dtor_none("function f() { $x = new class extends Nowhere {}; unset($x); }", "f");
}

/// Row 6.25: trait users, an inherited destructor, an interface's implementor.
#[test]
fn s6_trait_users_inherited_destructors_and_implementors_count() {
    dtor_gap("function f() { $u = new U; unset($u); }", "f");
    dtor_gap("function f(U $u) { $u = null; }", "f");
    dtor_gap("function f(UT $u) { $u = null; }", "f");
    dtor_gap("function f(CI $c) { $c = null; }", "f");
    dtor_gap("function f(I $i) { $i = null; }", "f");
    let parent = "abstract class P { public function __destruct() { echo '[P]'; } }\n\
        class C extends P { }\nfunction f(C $c) { $c = null; }";
    assert!(dtor(&drops_in(parent), "f"));
}

/// Row 6.25's last shape: the parent is in another file.
#[test]
fn s6_an_inherited_destructor_crosses_files() {
    let db = SteinsDatabase::default();
    let files = [
        ("p.php", "<?php\nabstract class P { public function __destruct() { echo '[P]'; } }\n"),
        ("c.php", "<?php\nclass C extends P {}\nfunction d(C $c) { $c = null; echo '<after>'; }\n"),
        ("n.php", "<?php\nclass Q {}\nclass N extends Q {}\nfunction e(N $n) { $n = null; }\n"),
    ];
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(path, text)| SourceFile::new(&db, (*path).to_owned(), (*text).to_owned()))
        .collect();
    let layout = steins_db::ProjectLayout::fallback();
    let project = Project::new(&db, inputs.clone(), layout, steins_db::PluginFacts::none());
    let gaps = |file: usize, symbol: &str| {
        let found = effect_summaries_project(&db, project, inputs[file]);
        let s = found.iter().find(|s| s.symbol == symbol).expect("a summary");
        assert_eq!(s.gaps.contains(&DESTRUCTOR), s.throws_gaps.contains(&DESTRUCTOR));
        s.gaps.contains(&DESTRUCTOR)
    };
    assert!(gaps(1, "d"));
    assert!(!gaps(2, "e"));
}

/// Row 6.26: `__call` does not answer `__destruct`.
#[test]
fn s6_a_magic_call_is_not_a_destructor() {
    dtor_none("function f() { $m = new M; unset($m); }", "f");
    dtor_none("function f(M $m) { $m = null; }", "f");
}

/// A loop runs a first write again: the previous value is dropped.
#[test]
fn s6_a_new_in_a_loop_drops_the_previous_iteration_s_value() {
    dtor_gap("function f($xs) { foreach ($xs as $x) { $d = new D; } }", "f");
    dtor_gap("function f() { while (true) { $d = new D; } }", "f");
}

/// Whether `symbol` of a whole file carries the destructor gap.
fn dtor_src(src: &str, symbol: &str) -> bool {
    dtor(&format!("<?php\n{src}\n"), symbol)
}

/// B1: an interface a *subclass* of the declaring class implements is an ancestor of a
/// class that inherits the destructor, so a value typed to it may run it (witnessed:
/// `[P]<body>`). The abstract middle class, a hop through a property and a trait-user
/// parent are the same shape; an interface nothing with a destructor implements is not.
#[test]
fn s6_an_interface_a_subclass_of_the_declaring_class_implements_is_reached() {
    let sub = "interface I {}\nclass P { public function __destruct() {} }\n\
        final class C extends P implements I {}\n";
    assert!(dtor_src(&format!("{sub}function g(I $i) {{ $i = null; }}"), "g"));
    assert!(dtor_src(&format!("{sub}function g(?I $i) {{ }}"), "g"));
    let hop = format!("{sub}final class H {{ public ?I $i = null; }}\nfunction g(H $h) {{ $h = null; }}");
    assert!(dtor_src(&hop, "g"));
    let mid = "abstract class Base {}\nclass P extends Base { public function __destruct() {} }\n\
        interface J {}\nabstract class Mid extends P implements J {}\nfinal class C extends Mid {}\n\
        function g(J $j) { $j = null; }";
    assert!(dtor_src(mid, "g"));
    let traity = "trait T { public function __destruct() {} }\nclass U { use T; }\ninterface I {}\n\
        final class V extends U implements I {}\nfunction g(I $i) { $i = null; }";
    assert!(dtor_src(traity, "g"));
    let none = format!("{sub}interface K {{}}\nfinal class Q implements K {{}}\n\
        function g(K $k) {{ $k = null; }}");
    assert!(!dtor_src(&none, "g"));
}

/// B2: a property that names its own class holds a bound, though the value it was
/// reached from is exact: `Node` the exact class has no destructor, a `DNode` in its
/// `?Node` property does (witnessed: `<body>[DNode]`).
#[test]
fn s6_a_self_typed_property_on_an_exact_new_asks_the_bound_question() {
    let src = "class Node { public ?Node $next = null;\n\
        public function __construct() { $this->next = new DNode(); } }\n\
        class DNode extends Node { public function __destruct() { echo '[DNode]'; } }\n\
        function f() { $n = new Node(); echo '<body>'; }";
    assert!(dtor_src(src, "f"));
    let plain = "class Node { public ?Node $next = null; }\nclass Leaf extends Node {}\n\
        function f() { $n = new Node(); }\nfunction g(Node $n) { }";
    assert!(!dtor_src(plain, "f"));
    assert!(!dtor_src(plain, "g"));
}

/// S6c (a), rows from the architect's consult on #915: an anonymous class lists its parent
/// and interfaces in the destructor closure only when its own body declares a destructor
/// (or imports a trait). The body is visible at the `new class`, so a clean one adds
/// nothing to what its parent reaches, where the proxy "whatever the body declares" made
/// every `Base $b` a gap as soon as one `new class extends Base {}` existed.
#[test]
fn s6c_an_anonymous_subclass_with_a_clean_body_adds_nothing_to_its_parent() {
    let clean = "class Plain {}\ninterface Face {}\n\
        function mk() { return new class extends Plain implements Face {}; }\n\
        function g(Plain $p) { $p = null; }\nfunction h(Face $f) { $f = null; }";
    assert!(!dtor_src(clean, "g"));
    assert!(!dtor_src(clean, "h"));
    // The anonymous class itself is its body's subject: a clean one runs nothing.
    assert!(!dtor_src("class Plain {}\nfunction f() { $x = new class extends Plain {}; unset($x); }", "f"));
}

/// S6c (a): an anonymous class that declares `__destruct` keeps its parent, and its
/// interfaces, in the closure, so a value typed to any of them may run it.
#[test]
fn s6c_an_anonymous_subclass_that_declares_a_destructor_keeps_its_parent_a_gap() {
    let dtor = "class Plain {}\ninterface Face {}\n\
        function mk() { return new class extends Plain implements Face { \
        public function __destruct() { echo '[anon]'; } }; }\n\
        function g(Plain $p) { $p = null; }\nfunction h(Face $f) { $f = null; }";
    assert!(dtor_src(dtor, "g"));
    assert!(dtor_src(dtor, "h"));
    // One in a method body, and one nested in a trait's method, are collected the same.
    let nested = "class Plain {}\ntrait Maker { public function mk() { \
        return new class extends Plain { public function __destruct() {} }; } }\n\
        function g(Plain $p) { $p = null; }";
    assert!(dtor_src(nested, "g"));
}

/// S1: a hint's class members count whatever else the union holds, and `self` and
/// `parent` name the class they stand in (witnessed: `[D]<body>`).
#[test]
fn s6_a_union_with_an_array_and_a_self_hint_are_read_by_their_classes() {
    assert!(dtor_src("final class D { public function __destruct() {} }\n\
        function g(array|D $x) { $x = null; }", "g"));
    assert!(dtor_src("final class D { public function __destruct() {} }\n\
        function g(int|string|null|D $x) { }", "g"));
    assert!(dtor_src("final class D { public function __destruct() {}\n\
        public function merge(self $o): void { $o = null; } }", "D::merge"));
    assert!(dtor_src("class B { public function __destruct() {} }\n\
        class K extends B { public function m(parent $p): void { } }", "K::m"));
    // The enclosing class reaches nothing, so neither does its `self`.
    assert!(!dtor_src("final class E { public function merge(self $o): void { $o = null; } }", "E::merge"));
    assert!(!dtor_src("function g(array|int|null $x) { $x = null; }", "g"));
}

/// S5: a class declared twice cannot be read, and may be the declaration that has the
/// destructor; so may a name no file declares (witnessed: `[P]<body>`; S6c).
#[test]
fn s6_a_class_declared_twice_may_run_a_destructor() {
    let twice = "class P { public function __destruct() { echo '[P]'; } }\n\
        if (PHP_VERSION_ID >= 80000) { final class C extends P {} } else { final class C extends P {} }\n\
        function g(C $c) { $c = null; }\nfunction h() { $c = new C(); echo '<body>'; }";
    assert!(dtor_src(twice, "g"));
    assert!(dtor_src(twice, "h"));
    let absent = "function g(\\Vendor\\Missing $m) { $m = null; }";
    assert!(dtor_src(absent, "g"));
}

/// S6c (b), rows from the consult on #915: a class that imports a trait counts only when
/// a trait it imports declares `__destruct`, aliases a method to it, or cannot be read.
/// A clean trait is the proxy's casualty: nothing runs at the drop (witnessed `<after>`),
/// so the drop reads as it would for a class with no trait.
#[test]
fn s6c_a_class_importing_only_clean_traits_runs_nothing() {
    let clean = "trait Clean { public function hello() { echo '[hello]'; } }\n\
        class U0 { use Clean; }\nclass U1 extends U0 {}\n\
        function a(U0 $u) { $u = null; }\nfunction b() { $u = new U0; unset($u); }\n\
        function c(U1 $u) { }\nfunction d() { $u = new U1; echo '<body>'; }";
    for symbol in ["a", "b", "c", "d"] {
        assert!(!dtor_src(clean, symbol), "{symbol}");
    }
    // A bound's subclass that imports only clean traits adds nothing to the closure.
    let sub = "trait Clean {}\nclass Base {}\nfinal class Sub extends Base { use Clean; }\n\
        interface Face {}\nfinal class Impl implements Face { use Clean; }\n\
        function a(Base $b) { $b = null; }\nfunction b(Face $f) { $f = null; }";
    assert!(!dtor_src(sub, "a"));
    assert!(!dtor_src(sub, "b"));
}

/// S6c (b): the traits that do bring one, each witnessed on PHP 8.5: a declared
/// `__destruct` (`[T2]<after>`), one a trait imports from another (`[T2]<after><caller>`),
/// and an alias at the class or the trait level (`[bye]<after>`: a method named
/// `__destruct` is the destructor wherever the alias is written).
#[test]
fn s6c_a_trait_that_declares_or_aliases_a_destructor_makes_its_users_a_gap() {
    let direct = "trait TD { public function __destruct() { echo '[TD]'; } }\n\
        class A { use TD; }\nclass A2 extends A {}\n\
        function f(A $u) { $u = null; }\nfunction g(A2 $u) { }\nfunction h() { $u = new A; }";
    for symbol in ["f", "g", "h"] {
        assert!(dtor_src(direct, symbol), "{symbol}");
    }
    let chain = "trait T2 { public function __destruct() {} }\ntrait T1 { use T2; }\n\
        trait T0 { use T1; }\nclass A { use T0; }\nfunction f(A $u) { $u = null; }";
    assert!(dtor_src(chain, "f"));
    let class_alias = "trait T { public function bye() { echo '[bye]'; } }\n\
        class A { use T { bye as __destruct; } }\nfunction f(A $u) { $u = null; }";
    assert!(dtor_src(class_alias, "f"));
    let loud = "trait T { public function bye() {} }\n\
        class A { use T { bye as protected __DESTRUCT; } }\nfunction f(A $u) { $u = null; }";
    assert!(dtor_src(loud, "f"));
    let trait_alias = "trait T { public function bye() {} }\ntrait T1 { use T { bye as __destruct; } }\n\
        class A { use T1; }\nfunction f(A $u) { $u = null; }";
    assert!(dtor_src(trait_alias, "f"));
    // An alias of another name is no destructor, and neither is an `insteadof`.
    let other = "trait T { public function bye() {} }\ntrait V { public function bye() {} }\n\
        class A { use T, V { T::bye insteadof V; bye as hello; } }\nfunction f(A $u) { $u = null; }";
    assert!(!dtor_src(other, "f"));
    // The bound's subclass is what brings it.
    let sub = "trait TD { public function __destruct() {} }\nclass Base {}\n\
        final class Sub extends Base { use TD; }\nfunction f(Base $b) { $b = null; }";
    assert!(dtor_src(sub, "f"));
}

/// S6c (b): a trait that cannot be read counts. The trait is declared nowhere (a vendor
/// trait the checkout lacks), twice, under a condition or through a `class_alias`, or
/// imports one of those, or holds a typed property (the hop reads a class's own only).
#[test]
fn s6c_a_trait_that_cannot_be_read_makes_its_users_a_gap() {
    let gap = |src: &str| assert!(dtor_src(src, "f"), "{src}");
    gap("class A { use \\Vendor\\T; }\nfunction f(A $u) { $u = null; }");
    gap("class A { use Missing; }\nfunction f(A $u) { $u = null; }");
    gap("trait T {}\ntrait T1 { use \\Vendor\\Inner; }\nclass A { use T1; }\nfunction f(A $u) { $u = null; }");
    gap("trait T {}\nclass A { use T, \\Vendor\\Other; }\nfunction f(A $u) { $u = null; }");
    // Declared twice: either may be the one that binds.
    gap("trait T {}\ntrait T { function __destruct() {} }\nclass A { use T; }\nfunction f(A $u) { $u = null; }");
    gap("trait T {}\ntrait T {}\nclass A { use T; }\nfunction f(A $u) { $u = null; }");
    // Declared under a condition: the declaration that binds is decided at run time.
    gap("if (PHP_VERSION_ID >= 80000) { trait T {} }\nclass A { use T; }\nfunction f(A $u) { $u = null; }");
    // A name a `class_alias` makes is its target's, which a name cannot say.
    gap("trait Real {}\nclass_alias('Real', 'Shadow');\nclass A { use Shadow; }\nfunction f(A $u) { $u = null; }");
}

/// S6c (b): a trait's properties are not lowered, so the typed-property hop reads their
/// hints off the trait, and a class that imports one holds what it holds (witnessed:
/// `[D]<after>` through `trait T { private ?D $d; }` and `class U { use T; }`). The hop
/// ends on a clean class, a scalar, an array, a static property and an engine class.
#[test]
fn s6c_a_typed_property_a_trait_imports_hops_like_the_class_s_own() {
    let hop = "class D { function __destruct() {} }\nclass Clean {}\n\
        trait Holds { private ?D $d = null; }\ntrait Quiet { private ?Clean $c = null; }\n\
        trait Nests { use Holds; }\n\
        trait Promotes { public function __construct(private D $d) {} }\n\
        class A { use Holds; }\nclass B extends A {}\nclass C { use Nests; }\n\
        class E { use Promotes; }\nclass F { use Quiet; }\n\
        function a(A $u) { $u = null; }\nfunction b(B $u) { }\nfunction c(C $u) { }\n\
        function e(E $u) { }\nfunction f(F $u) { $u = null; }\nfunction g() { $u = new A; }";
    for symbol in ["a", "b", "c", "e", "g"] {
        assert!(dtor_src(hop, symbol), "{symbol}");
    }
    assert!(!dtor_src(hop, "f"), "a clean class is none");
    let none = "trait T { private int $n = 0; private array $a = []; private static ?D $s = null;\n\
        private ?\\Closure $c = null; private \\DateTimeImmutable $d; }\n\
        class D { function __destruct() {} }\nclass A { use T; }\nfunction f(A $u) { $u = null; }";
    assert!(!dtor_src(none, "f"));
    // A property hinted with a class a cycle of traits and classes names ends the walk.
    let cycle = "trait T { private ?A $a = null; }\nclass A { use T; }\nfunction f(A $u) { $u = null; }";
    assert!(!dtor_src(cycle, "f"));
}

/// S6c (b): the trait graph resolves across files, and a trait another file adds or
/// removes changes the answer.
#[test]
fn s6c_a_trait_is_resolved_across_files() {
    let db = SteinsDatabase::default();
    let answer = |files: &[(&str, &str)], at: usize, symbol: &str| {
        let inputs: Vec<SourceFile> = files
            .iter()
            .map(|(path, text)| SourceFile::new(&db, (*path).to_owned(), (*text).to_owned()))
            .collect();
        let layout = steins_db::ProjectLayout::fallback();
        let project = Project::new(&db, inputs.clone(), layout, steins_db::PluginFacts::none());
        let found = effect_summaries_project(&db, project, inputs[at]);
        let s = found.iter().find(|s| s.symbol == symbol).expect("a summary");
        assert_eq!(s.gaps.contains(&DESTRUCTOR), s.throws_gaps.contains(&DESTRUCTOR));
        s.gaps.contains(&DESTRUCTOR)
    };
    let user = ("u.php", "<?php\nclass U { use \\Lib\\T; }\nfunction d(U $u) { $u = null; }\n");
    let clean = ("t.php", "<?php\nnamespace Lib;\ntrait T { public function x() {} }\n");
    let loud = ("t.php", "<?php\nnamespace Lib;\ntrait T { public function __destruct() {} }\n");
    assert!(answer(&[user], 0, "d"), "the trait is not in the universe");
    assert!(!answer(&[user, clean], 0, "d"), "the trait is read, and clean");
    assert!(answer(&[user, loud], 0, "d"), "the trait is read, and declares one");
}

/// S6c (b): an anonymous class that imports a trait follows the trait: a clean one adds
/// nothing to its parent, a trait that declares a destructor or cannot be read does.
#[test]
fn s6c_an_anonymous_class_importing_a_trait_follows_the_trait() {
    let clean = "class Plain {}\ntrait Clean {}\n\
        function mk() { return new class extends Plain { use Clean; }; }\n\
        function g(Plain $p) { $p = null; }";
    assert!(!dtor_src(clean, "g"));
    let loud = "class Plain {}\ntrait TD { function __destruct() {} }\n\
        function mk() { return new class extends Plain { use TD; }; }\n\
        function g(Plain $p) { $p = null; }";
    assert!(dtor_src(loud, "g"));
    let unseen = "class Plain {}\n\
        function mk() { return new class extends Plain { use \\Vendor\\T; }; }\n\
        function g(Plain $p) { $p = null; }";
    assert!(dtor_src(unseen, "g"));
    let aliased = "class Plain {}\ntrait T { function bye() {} }\n\
        function mk() { return new class extends Plain { use T { bye as __destruct; } }; }\n\
        function g(Plain $p) { $p = null; }";
    assert!(dtor_src(aliased, "g"));
    // A trait that itself imports an unread one.
    let nested = "class Plain {}\ntrait Outer { use \\Vendor\\Inner; }\n\
        function mk() { return new class extends Plain { use Outer; }; }\n\
        function g(Plain $p) { $p = null; }";
    assert!(dtor_src(nested, "g"));
    // The anonymous class as a subject: it is its trait.
    assert!(!dtor_src("trait Clean {}\nfunction f() { $x = new class { use Clean; }; unset($x); }", "f"));
    assert!(dtor_src("trait TD { function __destruct() {} }\nfunction f() { $x = new class { use TD; }; unset($x); }", "f"));
    assert!(dtor_src("trait T { function bye() {} }\nfunction f() { $x = new class { use T { bye as __destruct; } }; }", "f"));
    assert!(dtor_src("trait TD { function __destruct() {} }\nfunction f() { foo(new class { use TD; }); }\nfunction foo($x) {}", "f"));
}

/// S6c (c): a name no file declares and the engine does not may declare a destructor, as
/// every family that reads an unclosed chain has it: an `extends`, a parameter hint, a
/// typed property's hint, a `new`. Witnessed on PHP 8.5 with the vendor class loaded
/// (`[Vendor]<after>`, `<body>[Vendor]`).
#[test]
fn s6c_a_class_no_file_declares_may_run_a_destructor() {
    let gap = |src: &str, symbol: &str| assert!(dtor_src(src, symbol), "{symbol}: {src}");
    // The parent.
    let parent = "class C extends \\Vendor\\Base {}\nclass D2 extends C {}\n\
        function a(C $c) { $c = null; }\nfunction b() { $c = new C; unset($c); }\n\
        function c(D2 $c) { }";
    for symbol in ["a", "b", "c"] {
        gap(parent, symbol);
    }
    // The hint, by name and as a union member, and `new` of a class no file declares.
    gap("function f(\\Vendor\\Thing $t) { $t = null; }", "f");
    gap("function f(?\\Vendor\\Thing $t) { }", "f");
    gap("function f(int|\\Vendor\\Thing $t) { }", "f");
    // A `new` of a class whose chain ends at one: the `new` itself records no unknown-class gap.
    gap("class C extends \\Vendor\\Base {}\nfunction f() { $c = new C; echo '<body>'; }", "f");
    gap("class C extends \\Vendor\\Base {}\nfunction f() { foo(new C()); }\nfunction foo($x) {}", "f");
    // The typed property's hint, on an exact class and a bound one.
    let held = "final class H { private \\Vendor\\Thing $t; }\n\
        class G { protected ?\\Vendor\\Thing $t = null; }\n\
        function a(H $h) { $h = null; }\nfunction b() { $h = new H; }\nfunction c(G $g) { }";
    for symbol in ["a", "b", "c"] {
        gap(held, symbol);
    }
    // An unseen parent of an implementor: a value typed to the interface may be it.
    gap("interface Face {}\nfinal class Impl extends \\Vendor\\Base implements Face {}\n\
        function f(Face $f) { $f = null; }", "f");
    // A trait no file declares, on an anonymous class's binding.
    gap("function f() { $x = new class { use \\Vendor\\T; }; unset($x); }", "f");
    // A property a trait imports is hinted with a class no file declares.
    gap("trait T { private ?\\Vendor\\X $x = null; }\nclass A { use T; }\nfunction f(A $u) { $u = null; }", "f");
    // The same names, once a file declares them, read as any class does.
    let present = "namespace Vendor { class Base {} class Thing {} }\n\
        namespace App { class C extends \\Vendor\\Base {}\n\
        function a(C $c) { $c = null; }\nfunction b(\\Vendor\\Thing $t) { $t = null; } }";
    assert!(!dtor_src(present, "a") && !dtor_src(present, "b"));
}

/// S6c (c): what the engine declares is a closed chain, and so is a class chain that
/// ends at one. A name the namespace made up (`App\Exception` for an unimported
/// `Exception`) is not the engine's, and is unseen.
#[test]
fn s6c_an_engine_class_closes_the_chain() {
    let none = |src: &str, symbol: &str| assert!(!dtor_src(src, symbol), "{symbol}: {src}");
    none("class E extends \\Exception {}\nfunction f(E $e) { $e = null; }", "f");
    none("class A extends \\ArrayObject implements \\Countable {}\nfunction f(A $a) { }", "f");
    none("function f(\\Closure|\\Traversable|\\Stringable|\\DateTimeInterface $x) { $x = null; }", "f");
    none("function f() { $t = new \\ArrayObject([]); $d = new \\DateTimeImmutable(); }", "f");
    none("final class H { private \\DateTimeImmutable $d; private ?\\Closure $c = null; }\n\
        function f(H $h) { $h = null; }", "f");
    assert!(dtor_src("namespace App;\nfunction f(Exception $e) { $e = null; }", "f"), "App\\Exception is unseen");
}

/// S6c (c): the answer follows the universe across files: a vendor tree that holds the
/// class resolves it, and removing it makes it unseen.
#[test]
fn s6c_an_unseen_name_resolves_once_a_file_declares_it() {
    let db = SteinsDatabase::default();
    let answer = |files: &[(&str, &str)], at: usize, symbol: &str| {
        let inputs: Vec<SourceFile> = files
            .iter()
            .map(|(path, text)| SourceFile::new(&db, (*path).to_owned(), (*text).to_owned()))
            .collect();
        let layout = steins_db::ProjectLayout::fallback();
        let project = Project::new(&db, inputs.clone(), layout, steins_db::PluginFacts::none());
        let found = effect_summaries_project(&db, project, inputs[at]);
        let s = found.iter().find(|s| s.symbol == symbol).expect("a summary");
        assert_eq!(s.gaps.contains(&DESTRUCTOR), s.throws_gaps.contains(&DESTRUCTOR));
        s.gaps.contains(&DESTRUCTOR)
    };
    let user = ("u.php", "<?php\nfunction d(\\Lib\\Base $b) { $b = null; }\n");
    let clean = ("b.php", "<?php\nnamespace Lib;\nclass Base {}\n");
    let loud = ("b.php", "<?php\nnamespace Lib;\nclass Base { public function __destruct() {} }\n");
    assert!(answer(&[user], 0, "d"), "no file declares Base");
    assert!(!answer(&[user, clean], 0, "d"), "Base is declared and clean");
    assert!(answer(&[user, loud], 0, "d"), "Base is declared and has one");
}

/// S6c (c), the line it stops at: a `new` of a class no file declares records an
/// unknown-class gap at the `new` itself, so the body is `…?` through the same name and a
/// `destructor` gap on its drops would only repeat it. A hint that names such a class, and a
/// `new` of a declared class whose chain reaches one (which records no unknown-class gap), keep
/// the gap: there it is the only signal.
#[test]
fn s6c_a_new_of_an_unseen_class_is_left_to_its_unknown_class_gap() {
    let none = |src: &str| {
        let src = format!("<?php\n{src}\n");
        assert!(!dtor(&src, "f"), "{src}");
        let s = summary(&src, "f");
        assert!(s.gaps.contains(&"unknown-class") && !s.exhaustive, "unknown-class stays: {s:?}");
    };
    none("function f() { $t = new \\Vendor\\Thing(); echo '<body>'; }");
    none("function f() { $t = new \\Vendor\\Thing(); unset($t); }");
    none("function f() { foo(new \\Vendor\\Thing()); }\nfunction foo($x) {}");
    none("function f() { new \\Vendor\\Thing(); }");
    none("function f() { $x = new class extends Nowhere {}; unset($x); }");
    // The same names kept: a hint, a declared class over an unseen parent, an unseen trait.
    let gap = |src: &str| assert!(dtor_src(src, "f"), "{src}");
    gap("function f(\\Vendor\\Thing $t) { $t = null; }");
    gap("class C extends \\Vendor\\Base {}\nfunction f() { $c = new C; unset($c); }");
    gap("function f() { $x = new class { use \\Vendor\\T; }; unset($x); }");
}
