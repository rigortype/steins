//! Coercion at user boundaries (ADR-0099 §4.3's Coerce row, §4.6 and §7.5, issue #868): under
//! coercive typing an object handed to a user function's `string`-admitting parameter,
//! returned through a `string`-admitting return type, or assigned by a constructor to a
//! `string`-admitting typed property runs `__toString`. The witness table of the S5 slice,
//! each row run on PHP 8.5.11 (`witness` notes the observed output). A row reads in both
//! lanes, so a case states the effect lane's gap kinds and the throw lane's, sorted.
//!
//! The shared prelude's classes: `S` converts to a string (with output), `N` does not,
//! `Open` is a non-final converter, `Fin` a final one, `Foo` a plain class with the
//! subclass `FooS` that converts, `Inv` converts and is invokable, `Trav` converts and is
//! iterable.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{EffectSummary, effect_summaries_project, effect_summary};
use steins_syntax::SourceTree;

const CLASSES: &str = "\
    class S { public function __toString(): string { echo '[S]'; return 's'; } }\n\
    class N {}\n\
    class Open { public function __toString(): string { echo '[Open]'; return 'o'; } }\n\
    final class Fin { public function __toString(): string { echo '[Fin]'; return 'f'; } }\n\
    class Foo {}\n\
    class FooS extends Foo { public function __toString(): string { echo '[FooS]'; return 'fs'; } }\n\
    class Inv { public function __invoke() { return 1; } public function __toString(): string { echo '[Inv]'; return 'i'; } }\n\
    class Trav implements IteratorAggregate { public function getIterator(): Iterator { return new ArrayIterator([]); } public function __toString(): string { echo '[Trav]'; return 't'; } }\n";

/// The declarations the calls below name: one parameter type each.
const CALLEES: &str = "\
    function takes(string $s) { return $s; }\n\
    function takesNullable(?string $s) { return $s; }\n\
    function takesUnion(string|int $s) { return $s; }\n\
    function takesArray(string|array $s) { return $s; }\n\
    function takesIterable(string|iterable $s) { return $s; }\n\
    function takesCallable(string|callable $s) { return $s; }\n\
    function takesStringable(string|Stringable $s) { return $s; }\n\
    function takesObject(string|object $s) { return $s; }\n\
    function takesFoo(string|Foo $s) { return $s; }\n\
    function takesMixed(mixed $m) { return $m; }\n\
    function takesUntyped($m) { return $m; }\n\
    function takesInt(int $i) { return $i; }\n\
    function takesFloat(float $f) { return $f; }\n\
    function takesBool(bool $b) { return $b; }\n\
    function takesVariadic(string ...$parts) { return $parts; }\n\
    function takesRef(string &$s) { return $s; }\n\
    function two(int $a, string $b) { return $b; }\n\
    final class Holder {\n\
        public string $name = '';\n\
        public function __construct(string $n) { $this->name = $n; }\n\
        public function set(string $n) { return $n; }\n\
        public static function make(string $n) { return $n; }\n\
    }\n\
    final class Promo { public function __construct(public string $p) {} }\n\
    class Base { public function __construct(public string $p = '') {} }\n\
    class Child extends Base { public function __construct($x) { parent::__construct($x); } }\n\
    final class FinN {}\n\
    final class PropS { public string $name = ''; public function __construct(Fin $s) { $this->name = $s; } }\n\
    final class PropN { public string $name = ''; public function __construct(FinN $n) { $this->name = $n; } }\n\
    final class PropUnknown { public string $name = ''; public function __construct($x) { $this->name = $x; } }\n\
    final class PropBound { public string $name = ''; public function __construct(Open $o) { $this->name = $o; } }\n\
    final class PropOpenN { public string $name = ''; public function __construct(N $n) { $this->name = $n; } }\n\
    final class PropInt { public int $n = 0; public function __construct(Fin $s) { $this->n = $s; } }\n";

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

/// The file every case runs in: coercive, or `declare(strict_types=1)`.
fn file(strict: bool, code: &str) -> String {
    let declare = if strict { "declare(strict_types=1);\n" } else { "" };
    format!("<?php\n{declare}{CLASSES}{CALLEES}{code}\n")
}

/// The effect summary of `symbol` in a coercive file, its gap kinds sorted per lane.
fn lanes(code: &str, symbol: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    lanes_in(&file(false, code), symbol)
}

fn lanes_in(src: &str, symbol: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let s = summary(src, symbol);
    let sorted = |mut kinds: Vec<&'static str>| {
        kinds.sort_unstable();
        kinds
    };
    assert_eq!(s.exhaustive, s.gaps.is_empty(), "{s:?}");
    assert_eq!(s.throws_exhaustive, s.throws_gaps.is_empty(), "{s:?}");
    (sorted(s.gaps), sorted(s.throws_gaps))
}

const TO_STRING: &str = "operator-to-string";

/// `symbol` reads exhaustive in both lanes and proves no effect label.
fn nothing(code: &str, symbol: &str) {
    assert_eq!(lanes(code, symbol), (vec![], vec![]), "{symbol}\n{code}");
    assert!(summary(&file(false, code), symbol).labels.is_empty(), "{symbol}\n{code}");
}

/// `symbol` reads exhaustive and proves the conversion's output: an edge to `__toString`.
fn runs_to_string(code: &str, symbol: &str) {
    assert_eq!(lanes(code, symbol), (vec![], vec![]), "{symbol}\n{code}");
    let labels = summary(&file(false, code), symbol).labels;
    assert_eq!(labels, ["io.output.buffer"], "{symbol}\n{code}");
}

/// `symbol` has the to-string gap in both lanes, whatever else it has.
fn has_gap(code: &str, symbol: &str) {
    let (effects, throws) = lanes(code, symbol);
    assert!(
        effects.contains(&TO_STRING) && throws.contains(&TO_STRING),
        "{symbol}: {effects:?} {throws:?}\n{code}"
    );
}

/// `symbol` has no to-string gap in either lane, whatever else it has.
fn lacks_gap(code: &str, symbol: &str) {
    let (effects, throws) = lanes(code, symbol);
    assert!(
        !effects.contains(&TO_STRING) && !throws.contains(&TO_STRING),
        "{symbol}: {effects:?} {throws:?}\n{code}"
    );
}

/// `symbol` has exactly the to-string gap in both lanes.
fn gap(code: &str, symbol: &str) {
    assert_eq!(lanes(code, symbol), (vec![TO_STRING], vec![TO_STRING]), "{symbol}\n{code}");
}

// ---- 5.1-5.2 an exact class handed to `string` ---------------------------------------

#[test]
fn an_exact_class_with_to_string_handed_to_a_string_parameter_is_an_edge() {
    // 5.1. witness: `[S]` `'s'`.
    runs_to_string("function f() { return takes(new S); }", "f");
    runs_to_string("function f() { return takes(new Fin); }", "f");
}

#[test]
fn an_exact_class_without_to_string_handed_to_a_string_parameter_runs_nothing() {
    // 5.2, must stay. witness: `TypeError`, no user code.
    nothing("function f() { return takes(new N); }", "f");
}

#[test]
fn a_bound_class_handed_to_a_string_parameter_is_a_gap_unless_its_conversion_is_final() {
    // 5.3. witness: `[Open]`.
    gap("function f(Open $o) { return takes($o); }", "f");
    runs_to_string("function f(Fin $o) { return takes($o); }", "f");
}

// ---- 5.4-5.8 types that never convert, and types that do -----------------------------

#[test]
fn a_stringable_or_object_or_mixed_hint_never_converts() {
    // 5.4, 5.5, 5.8, must stay. witness: `'obj'` for `S`, `TypeError` for `N` (`Stringable`).
    for call in [
        "takesStringable(new S)",
        "takesStringable(new N)",
        "takesObject(new S)",
        "takesMixed(new S)",
        "takesUntyped(new S)",
    ] {
        nothing(&format!("function f() {{ return {call}; }}"), "f");
    }
    // The same for an operand nothing is known of: the object is taken as it is.
    for call in ["takesStringable($x)", "takesObject($x)", "takesMixed($x)", "takesUntyped($x)"] {
        nothing(&format!("function f($x) {{ return {call}; }}"), "f");
    }
}

#[test]
fn a_hint_with_no_string_member_converts_nothing() {
    // 5.6, must stay. witness: `TypeError` for each.
    for call in ["takesInt(new S)", "takesFloat(new S)", "takesBool(new S)"] {
        nothing(&format!("function f() {{ return {call}; }}"), "f");
        nothing(&format!("function f($x) {{ return {call}; }}"), "f");
    }
}

#[test]
fn every_hint_that_admits_string_and_not_the_object_converts() {
    // 5.7. witness: `[S]` each. `string|array`, `string|iterable` and `string|callable`
    // lower to no `NativeType` yet convert.
    for call in [
        "takesNullable(new S)",
        "takesUnion(new S)",
        "takesArray(new S)",
        "takesIterable(new S)",
        "takesCallable(new S)",
        "takesFoo(new S)",
    ] {
        runs_to_string(&format!("function f() {{ return {call}; }}"), "f");
        gap(&format!("function f($x) {{ return {}; }}", call.replace("new S", "$x")), "f");
    }
}

#[test]
fn an_object_that_is_a_member_of_the_hint_is_taken_as_it_is() {
    // Neighbours. witness: `'obj'` with no output for each.
    nothing("function f() { return takesFoo(new Foo); }", "f");
    nothing("function f() { return takesFoo(new FooS); }", "f");
    nothing("function f() { return takesCallable(new Inv); }", "f");
    nothing("function f() { return takesIterable(new Trav); }", "f");
    // A bound that may be a `Foo` or may not converts nothing unless it is not a `Foo`.
    nothing("function f(FooS $o) { return takesFoo($o); }", "f");
}

#[test]
fn self_and_parent_in_a_hint_name_the_declaring_class_and_its_parent() {
    // Neighbour (symfony's `TreeNode::addChild(self|string|callable $node)`). witness: an
    // instance of the class is taken as it is; any other `Stringable` object is converted.
    let tree = "final class Tree {\n\
        public function add(self|string $n) { return $n; }\n\
        public function convertOnly(string|int $n) { return $n; }\n\
    }\n\
    class Leaf extends Foo {\n\
        public function add(parent|string $n) { return $n; }\n\
    }";
    nothing(&format!("{tree}\nfunction f() {{ return (new Tree)->add(new Tree); }}"), "f");
    nothing(&format!("{tree}\nfunction f() {{ return (new Leaf)->add(new FooS); }}"), "f");
    // An object that is neither converts (the `Stringable` one) or raises.
    let converted = format!("{tree}\nfunction f() {{ return (new Tree)->add(new S); }}");
    runs_to_string(&converted, "f");
    runs_to_string(
        &format!("{tree}\nfunction f() {{ return (new Tree)->convertOnly(new S); }}"),
        "f",
    );
}

// ---- 5.9-5.10 what the operand is ----------------------------------------------------

#[test]
fn an_operand_shown_not_to_be_an_object_converts_nothing() {
    // 5.9, must stay. witness: `'ax'`.
    nothing("function f(string $x) { return takes('a' . $x); }", "f");
    nothing("function f() { return takes('a'); }", "f");
    nothing("function f() { return takes(1.5); }", "f");
    nothing("function f(int $i) { return takes($i); }", "f");
    nothing("function f(?string $s) { return takes($s); }", "f");
    // A call whose declared return is a scalar (S8's classifier).
    lacks_gap("function f($x) { return takes(trim($x)); }", "f");
    nothing("function f() { return takes(trim('x')); }", "f");
    nothing("function g(): string { return 'x'; }\nfunction f() { return takes(g()); }", "f");
    // An array is a `TypeError`, not a conversion (witness).
    nothing("function f() { return takes([new S]); }", "f");
}

#[test]
fn a_match_whose_every_arm_holds_no_object_converts_nothing() {
    // Neighbour (a `match` of literals is the commonest `: ?string` return in the corpora).
    // witness: `'png'`, `'jpg'` or `NULL`.
    nothing(
        "function r(string $d): ?string { return match (true) {\n\
            str_starts_with($d, 'PNG') => 'png',\n\
            default => null,\n\
        }; }",
        "r",
    );
    nothing(
        "function f($x) { return takes(match ($x) { 1 => 'a', 2 => 'b', default => throw new \\Exception('x') }); }",
        "f",
    );
    // An arm that may be an object keeps the site.
    has_gap("function f($x, $o) { return takes(match ($x) { 1 => 'a', default => $o }); }", "f");
}

#[test]
fn an_operand_nothing_is_known_of_is_a_gap() {
    // 5.10, the volume driver. witness: `[S]` when handed an `S`.
    gap("function f($x) { return takes($x); }", "f");
    gap("function f() { $x = new S; return takes($x); }", "f");
    gap("function f() { return takes(true ? new S : 'a'); }", "f");
    has_gap("function f($x) { return takes(clone $x); }", "f");
    gap("function f(object $o) { return takes($o); }", "f");
    // A parameter some call in the frame may rewrite is no longer what it was declared.
    has_gap("function f(string $s) { sort($s); return takes($s); }", "f");
}

// ---- 5.11 methods, constructors and promotion ----------------------------------------

#[test]
fn a_constructor_a_method_and_a_promoted_parameter_convert_their_arguments() {
    // 5.11. witness: `[S]` each.
    runs_to_string("function f() { return new Holder(new S); }", "f");
    runs_to_string("function f() { return (new Holder('a'))->set(new S); }", "f");
    runs_to_string("function f() { return Holder::make(new S); }", "f");
    runs_to_string("function f() { return new Promo(new S); }", "f");
    gap("function f($x) { return new Holder($x); }", "f");
    nothing("function f() { return new Holder(new N); }", "f");
}

#[test]
fn a_parent_constructor_call_converts_its_argument() {
    // Neighbour: `parent::__construct($x)` runs the parent's parameter type.
    gap("function f() { return new Child(new S); }", "Child::__construct");
}

// ---- 5.12-5.14 returns ---------------------------------------------------------------

#[test]
fn a_return_of_an_object_through_a_string_type_is_an_edge() {
    // 5.12. witness: `[S]`.
    runs_to_string("function r(): string { return new S; }", "r");
    runs_to_string("function r(): ?string { return new S; }", "r");
    runs_to_string("function r(): string|int { return new S; }", "r");
    // Closures and arrow functions convert through their own return types (witnessed).
    runs_to_string("function r() { $c = function (): string { return new S; }; return $c(); }", "r");
    runs_to_string("function r() { $c = fn(): string => new S; return $c(); }", "r");
}

#[test]
fn a_return_of_an_unknown_operand_through_a_string_type_is_a_gap() {
    // 5.13. witness: `[S]` when handed an `S`.
    gap("function r($x): string { return $x; }", "r");
    gap("function r(Open $o): ?string { return $o; }", "r");
    gap("class K { public function r($x): string { return $x; } }", "K::r");
}

#[test]
fn a_return_that_converts_nothing_stays_exhaustive() {
    // 5.14, must stay. witness: `'ax'`, `5`, the object itself with nothing printed.
    nothing("function r(string $x): string { return 'a' . $x; }", "r");
    nothing("function r(): int { return 5; }", "r");
    nothing("function r(): string|Stringable { return new S; }", "r");
    nothing("function r(): object { return new S; }", "r");
    nothing("function r(): mixed { return new S; }", "r");
    nothing("function r(): S { return new S; }", "r");
    nothing("function r($x): int { return $x; }", "r");
    nothing("function r(): ?string { return null; }", "r");
    nothing("function r(): string { return new N; }", "r");
    // `$this->name` of a typed property is as the property is declared.
    nothing(
        "class K { public string $n = ''; public function r(): string { return $this->n; } }",
        "K::r",
    );
}

// ---- 5.15-5.17 a constructor's typed property ----------------------------------------

#[test]
fn a_constructor_write_of_an_object_to_a_string_property_is_an_edge() {
    // 5.15. witness: `[S]`.
    let s = summary(&file(false, ""), "PropS::__construct");
    assert!(s.exhaustive && s.throws_exhaustive, "{s:?}");
    assert_eq!(s.labels, ["io.output.buffer"]);
}

#[test]
fn a_constructor_write_that_raises_converts_nothing() {
    // 5.16, must stay. witness: `TypeError`, no user code.
    for class in ["PropN", "PropInt"] {
        let s = summary(&file(false, ""), &format!("{class}::__construct"));
        assert!(s.exhaustive && s.throws_exhaustive && s.labels.is_empty(), "{class}: {s:?}");
    }
}

#[test]
fn a_constructor_write_of_an_unknown_operand_to_a_string_property_is_a_gap() {
    // 5.17. witness: `[S]` and `[Open]`.
    // `PropOpenN` takes a non-final `N`, whose subclass may convert: a bound, so a gap.
    for class in ["PropUnknown", "PropBound", "PropOpenN"] {
        let s = summary(&file(false, ""), &format!("{class}::__construct"));
        assert_eq!((s.gaps, s.throws_gaps), (vec![TO_STRING], vec![TO_STRING]), "{class}");
    }
}

#[test]
fn only_a_constructor_write_is_a_site() {
    // Outside a constructor the write is a structural state construct already (effect lane),
    // and the throw lane is not asked: the slice covers constructors only.
    let src = "class K { public string $n = ''; public function set(S $s) { $this->n = $s; } }";
    let s = summary(&file(false, src), "K::set");
    assert!(!s.gaps.contains(&TO_STRING) && !s.throws_gaps.contains(&TO_STRING), "{s:?}");
}

#[test]
fn a_property_the_chain_does_not_declare_typed_is_not_converted() {
    // Untyped, undeclared and an ancestor's private property store the object as it is.
    for class in [
        "class K { public $n; public function __construct($x) { $this->n = $x; } }",
        "class K { public function __construct($x) { $this->n = $x; } }",
        "class A { private string $n = ''; }\nclass K extends A { public function __construct($x) { $this->n = $x; } }",
    ] {
        let s = summary(&file(false, class), "K::__construct");
        assert!(!s.gaps.contains(&TO_STRING), "{class}: {s:?}");
    }
    // An inherited, visible, typed property is read off its declaring class.
    let inherited = "class A { protected string $n = ''; }\n\
        class K extends A { public function __construct($x) { $this->n = $x; } }";
    let s = summary(&file(false, inherited), "K::__construct");
    assert!(s.gaps.contains(&TO_STRING), "{s:?}");
}

// ---- 5.18 strict files ---------------------------------------------------------------

#[test]
fn a_strict_file_has_no_coercion_site() {
    // 5.18, must stay. witness: `TypeError`, no user code, in every row.
    for (code, symbol) in [
        ("function f() { return takes(new S); }", "f"),
        ("function f($x) { return takes($x); }", "f"),
        ("function r(): string { return new S; }", "r"),
        ("function r($x): string { return $x; }", "r"),
        ("function f() { return new Holder(new S); }", "f"),
        ("function f($x) { return (new Holder('a'))->set($x); }", "f"),
        ("function f() { return takesVariadic(new S, $x); }", "f"),
    ] {
        let s = summary(&file(true, code), symbol);
        assert!(
            s.exhaustive && s.throws_exhaustive && s.labels.is_empty(),
            "{symbol} in a strict file: {s:?}\n{code}"
        );
    }
    let s = summary(&file(true, ""), "PropS::__construct");
    assert!(s.exhaustive && s.throws_exhaustive && s.labels.is_empty(), "{s:?}");
    let s = summary(&file(true, ""), "PropUnknown::__construct");
    assert!(s.exhaustive && s.throws_exhaustive, "{s:?}");
}

// ---- argument forms ------------------------------------------------------------------

#[test]
fn a_named_argument_is_read_against_its_own_parameter() {
    // Neighbour. witness: `[S]`.
    runs_to_string("function f() { return takes(s: new S); }", "f");
    runs_to_string("function f() { return two(b: new S, a: 1); }", "f");
    // The parameter it names is `int`: nothing converts.
    nothing("function f() { return two(a: new S, b: 'x'); }", "f");
    gap("function f($x) { return two(b: $x, a: 1); }", "f");
}

#[test]
fn a_positional_argument_is_read_against_the_parameter_at_its_position() {
    // Neighbour. witness: `[S]`.
    runs_to_string("function f() { return two(1, new S); }", "f");
    nothing("function f() { return two(new S, 'x'); }", "f");
    // An extra argument binds no parameter.
    nothing("function f() { return takes('a', new S); }", "f");
}

#[test]
fn a_variadic_parameter_converts_every_argument_it_collects() {
    // Neighbour. witness: `[S][S]`.
    runs_to_string("function f() { return takesVariadic('a', new S); }", "f");
    gap("function f($x) { return takesVariadic('a', 'b', $x); }", "f");
    nothing("function f() { return takesVariadic('a', 'b'); }", "f");
}

#[test]
fn a_spread_argument_is_a_gap_unless_it_holds_no_object() {
    // Neighbour. witness: `[S]` for `takes(...[new S])`.
    has_gap("function f($xs) { return takes(...$xs); }", "f");
    has_gap("function f(array $xs) { return takes(...$xs); }", "f");
    nothing("function f() { return takes(...['a']); }", "f");
    // No parameter at or after the spread converts.
    lacks_gap("function f($xs) { return takesInt(...$xs); }", "f");
}

#[test]
fn a_by_reference_parameter_converts_a_variable_it_is_handed() {
    // Neighbour. witness: `[S]`.
    gap("function f() { $o = new S; return takesRef($o); }", "f");
}

#[test]
fn a_callee_with_no_project_declaration_converts_nothing_here() {
    // A builtin's parameters are the catalog's (ADR-0021), an unknown function a gap already.
    let (effects, _) = lanes("function f($x) { return strlen($x); }", "f");
    assert_eq!(effects, ["user-code-reach"]);
}

// ---- composition with the other sites -------------------------------------------------

#[test]
fn a_conversion_the_operand_already_made_is_not_reported_twice() {
    // `.` converts `$o` once, in the operator; the call receives a string.
    let (effects, throws) = lanes("function f(Open $o) { return takes('a' . $o); }", "f");
    assert_eq!((effects, throws), (vec![TO_STRING], vec![TO_STRING]));
    let (effects, throws) = lanes("function f(Open $o) { return takes((string) $o); }", "f");
    assert_eq!((effects, throws), (vec![TO_STRING], vec![TO_STRING]));
    // A return of a concatenation carries the operator's gap and no second one.
    let (effects, _) = lanes("function r(Open $o): string { return 'a' . $o; }", "r");
    assert_eq!(effects, [TO_STRING]);
    // The cast converts once, in the operator, and the call receives a string.
    runs_to_string("function f() { return takes((string) new S); }", "f");
}

#[test]
fn a_float_handed_to_a_string_parameter_adds_no_setting_label() {
    // ADR-0101 D4 keeps the operator-site `precision` read deferred; the boundary does not
    // add it (witness: `takes(1.5)` is `'1.5'`).
    let s = summary(&file(false, "function f() { return takes(1.5); }"), "f");
    assert!(s.labels.is_empty() && s.exhaustive, "{s:?}");
}

// ---- 5.19-5.21 across files ----------------------------------------------------------

/// The summary of `symbol` in `files[at]`, the project being `files`.
fn project_summary(files: &[(&str, &str)], at: usize, symbol: &str) -> EffectSummary {
    let db = SteinsDatabase::default();
    let inputs: Vec<SourceFile> = files
        .iter()
        .map(|(path, text)| SourceFile::new(&db, (*path).to_owned(), (*text).to_owned()))
        .collect();
    let layout = steins_db::ProjectLayout::fallback();
    let project = Project::new(&db, inputs.clone(), layout, steins_db::PluginFacts::none());
    let found = effect_summaries_project(&db, project, inputs[at]);
    found.into_iter().find(|s| s.symbol == symbol).expect("a summary")
}

const S_CLASS: &str =
    "class S { public function __toString(): string { echo '[S]'; return 's'; } }\n";

#[test]
fn the_calling_files_mode_governs_a_parameter() {
    // 5.19. witness: a coercive caller into a strict callee prints `[S]`.
    let lib = "<?php\ndeclare(strict_types=1);\nfunction takes_strict_lib(string $s) { return $s; }\n";
    let caller = format!("<?php\n{S_CLASS}function f() {{ return takes_strict_lib(new S); }}\n");
    let s = project_summary(&[("caller.php", &caller), ("lib.php", lib)], 0, "f");
    assert!(s.exhaustive && s.throws_exhaustive, "{s:?}");
    assert_eq!(s.labels, ["io.output.buffer"]);
}

#[test]
fn a_strict_caller_into_a_coercive_callee_converts_nothing() {
    // 5.20, must stay. witness: `TypeError`.
    let lib = "<?php\nfunction takes_coercive_lib(string $s) { return $s; }\n";
    let caller = format!(
        "<?php\ndeclare(strict_types=1);\n{S_CLASS}function f() {{ return takes_coercive_lib(new S); }}\n"
    );
    let s = project_summary(&[("caller.php", &caller), ("lib.php", lib)], 0, "f");
    assert!(s.exhaustive && s.throws_exhaustive && s.labels.is_empty(), "{s:?}");
}

#[test]
fn the_declaring_files_mode_governs_a_return() {
    // 5.21. witness: the conversion runs inside the coercive callee. The callee carries the
    // gap, and the strict caller reaches it through the edge.
    let lib = "<?php\nfunction returns_from_coercive_lib($x): string { return $x; }\n";
    let caller = format!(
        "<?php\ndeclare(strict_types=1);\n{S_CLASS}function f() {{ return returns_from_coercive_lib(new S); }}\n"
    );
    let files = [("caller.php", caller.as_str()), ("lib.php", lib)];
    let callee = project_summary(&files, 1, "returns_from_coercive_lib");
    assert_eq!(callee.gaps, [TO_STRING]);
    assert_eq!(callee.throws_gaps, [TO_STRING]);
    let at_caller = project_summary(&files, 0, "f");
    assert!(!at_caller.exhaustive && !at_caller.throws_exhaustive, "{at_caller:?}");
    // The reverse: a coercive caller into a strict callee's return has no site in the callee.
    let strict_lib = "<?php\ndeclare(strict_types=1);\nfunction returns_from_strict_lib($x): string { return $x; }\n";
    let callee = project_summary(&[("lib.php", strict_lib)], 0, "returns_from_strict_lib");
    assert!(callee.exhaustive && callee.throws_exhaustive, "{callee:?}");
}

#[test]
fn a_parameter_type_is_read_in_the_callees_namespace() {
    // `Stringable` unqualified in a namespace is `Ns\Stringable`: no such class, so the
    // object is not one, and converts. Imported, it is the engine's and never converts.
    let lib = "<?php\nnamespace Lib;\nfunction a(string|Stringable $s) { return $s; }\n";
    let imported = "<?php\nnamespace Lib;\nuse Stringable;\nfunction b(string|Stringable $s) { return $s; }\n";
    let qualified = "<?php\nnamespace Lib;\nfunction c(string|\\Stringable $s) { return $s; }\n";
    let caller = format!(
        "<?php\n{S_CLASS}function fa() {{ return \\Lib\\a(new S); }}\n\
         function fb() {{ return \\Lib\\b(new S); }}\nfunction fc() {{ return \\Lib\\c(new S); }}\n"
    );
    let at = |lib: &str, symbol: &str| {
        project_summary(&[("caller.php", &caller), ("lib.php", lib)], 0, symbol).labels
    };
    assert_eq!(at(lib, "fa"), ["io.output.buffer"]);
    assert!(at(imported, "fb").is_empty());
    assert!(at(qualified, "fc").is_empty());
}
