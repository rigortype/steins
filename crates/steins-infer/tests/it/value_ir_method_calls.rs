//! The value IR carries method calls (issue #386, ADR-0075 §3 as amended).
//!
//! `$b->m()` written as an argument, in a dump, inside an array or nested one call
//! deep now resolves exactly as `$v = $b->m();` does — one target resolution, one
//! body walk, one memo entry shared with the statement form. This file pins the
//! parity, the one-walk property, and every decline the amendment states: the
//! object fence (ADR-0057 B5), `Receiver::Prop` (ADR-0052 §7), nullsafe, and the
//! frame-less seams.
//!
//! Facts are observed the way the heap tests observe them — a typed sink under
//! `strict_types=1`, which fires only if the value crossed — and by `dumpType`.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, ID, NoFold, check_project};

fn findings(src: &str) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let input = SourceFile::new(&db, "main.php".to_owned(), src.to_owned());
    let project = Project::new(
        &db,
        vec![input],
        steins_db::ProjectLayout::fallback(),
        steins_db::PluginFacts::none(),
    );
    check_project(&db, project, &mut NoFold)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn count(src: &str) -> usize {
    findings(src).len()
}

fn dumped(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one dump, got {ds:?}");
    ds[0].message.clone()
}

/// The box the receiver leg's own fixtures use (`call_site_heap_entry.rs`), so the
/// two files observe one shape from two positions. Deliberately not `final`: an
/// allocation-proven receiver dispatches exactly without it.
const BOXM: &str = "<?php\ndeclare(strict_types=1);\n\
    function needInt(int $x): void {}\n\
    function needString(string $s): void {}\n\
    class Box {\n\
    \x20 public function __construct(public mixed $value) {}\n\
    \x20 public function unwrap(): mixed { return $this->value; }\n\
    \x20 public function get(): mixed { return $this->value; }\n\
    }\n";

// ---------------------------------------------------------------------------
// The parity: argument and dump position answer what the assignment answers.
// ---------------------------------------------------------------------------

#[test]
fn a_method_call_in_argument_position_is_the_value_its_summary_proves() {
    // The pin `call_site_heap_entry.rs` left as the value-IR limit, flipped: the
    // receiver's copy proves `1` inside `unwrap`, and the sink now sees it.
    let direct = format!("{BOXM}$b = new Box(1);\nneedString($b->unwrap());\n");
    let f = findings(&direct);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);

    // Parity with the function twin of the same body, which has always answered.
    let function = format!(
        "{BOXM}function unwrap(Box $b): mixed {{ return $b->value; }}\n\
         $b = new Box(1);\nneedString(unwrap($b));\n"
    );
    let g = findings(&function);
    assert_eq!(g.len(), 1, "{g:#?}");
    assert_eq!(f[0].id, g[0].id, "the two forms report the same finding");

    // And the finding follows the value, not the shape.
    assert_eq!(count(&format!("{BOXM}$b = new Box('s');\nneedString($b->unwrap());\n")), 0);
}

#[test]
fn a_method_call_in_dump_position_reads_the_summary() {
    assert_eq!(
        dumped(&format!("{BOXM}$b = new Box(1);\n\\PHPStan\\dumpType($b->get());")),
        "dumped type: 1",
    );
    // The assignment form, for the parity claim.
    assert_eq!(
        dumped(&format!("{BOXM}$b = new Box(1);\n$v = $b->get();\n\\PHPStan\\dumpType($v);")),
        "dumped type: 1",
    );
}

#[test]
fn a_method_call_nested_in_a_function_call_crosses_two_boundaries() {
    // `f($b->m())` binds `f`'s parameter from the method's summary, then `f`'s own
    // summary reaches the sink — the nested-argument seam (`nested_call_singleton`).
    let src = format!(
        "{BOXM}function f(mixed $v): mixed {{ return $v; }}\n\
         $b = new Box(1);\nneedString(f($b->unwrap()));\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
    assert_eq!(
        count(&format!(
            "{BOXM}function f(mixed $v): mixed {{ return $v; }}\n\
             $b = new Box('s');\nneedString(f($b->unwrap()));\n"
        )),
        0,
    );
}

#[test]
fn a_static_call_in_argument_position_resolves_through_its_class() {
    // `Foo::m(1)` resolves by `resolve_static_named` — a named class, no receiver.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Foo { public static function m(int $n): mixed { return $n; } }\n\
        needString(Foo::m(1));\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
    assert_eq!(
        dumped("<?php\nclass Foo { public static function m(int $n): mixed { return $n; } }\n\\PHPStan\\dumpType(Foo::m(1));"),
        "dumped type: 1",
    );
}

#[test]
fn a_this_receiver_resolves_where_the_frame_is_in_hand() {
    // Inside a method, `$this->m()` and `self::m()` in value position: the dump
    // surface and the propagated check both carry the caller's frame, so the
    // receiver resolves (under the final/private override guard for a non-exact
    // `$this` — the class is `final` here so the guard admits it).
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        final class C {\n\
        \x20 public function m(int $n): mixed { return $n; }\n\
        \x20 public static function s(int $n): mixed { return $n; }\n\
        \x20 public function run(): void { needString($this->m(1)); }\n\
        \x20 public function runStatic(): void { needString(self::s(1)); }\n\
        }\n";
    let f = findings(src);
    assert_eq!(f.len(), 2, "both receivers fire: {f:#?}");
    assert!(f.iter().all(|d| d.id == ID));

    let dump = "<?php\nfinal class C {\n\
        \x20 public function m(int $n): mixed { return $n; }\n\
        \x20 public function run(): void { \\PHPStan\\dumpType($this->m(1)); }\n\
        }\n";
    assert_eq!(dumped(dump), "dumped type: 1");
}

#[test]
fn a_receiver_position_new_reads_the_constructor_summary() {
    // ADR-0057 C7's third seam: `Receiver::New` carries its arguments now, so the
    // receiver object is minted here, its constructor walked, and the method
    // dispatched on what that constructor left behind.
    assert_eq!(
        dumped(&format!("{BOXM}\\PHPStan\\dumpType((new Box(1))->get());")),
        "dumped type: 1",
    );
    // A constructor that WRITES the property rather than promoting it — the #385
    // summary, read at the receiver.
    let written = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class B {\n\
        \x20 public mixed $value = null;\n\
        \x20 public function __construct(int $v) { $this->value = $v; }\n\
        \x20 public function get(): mixed { return $this->value; }\n\
        }\n\
        needString((new B(1))->get());\n";
    let f = findings(written);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn a_method_call_element_makes_its_array_a_shape_rather_than_nothing() {
    // The lowering stopped collapsing `[$o->m(), 2]` (issue #386), so the array's
    // own consumers see the keys and the count they always could have.
    let src = format!(
        "{BOXM}$b = new Box(1);\n$a = [$b->get(), 2];\n\\PHPStan\\dumpType($a);"
    );
    let d = dumped(&src);
    assert!(d.contains('2'), "the sibling element is known: {d}");
    assert_ne!(d, "dumped type: unknown", "the array no longer collapses whole");
}

#[test]
fn returning_a_method_call_still_composes_its_heap_summary() {
    // `return $o->m()` is served by the statement's own summary through
    // `return_heap_object`'s wildcard (ADR-0057 §2.3's composition arm). The new
    // carrier must not shadow that arm with a value-lane read — an object has no
    // value component, so a `MethodCall` arm there would have replaced a working
    // crossing with `None`.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Foo { public function __construct(public mixed $n) {} }\n\
        final class Maker { public function make(mixed $n): Foo { return new Foo($n); } }\n\
        function outer(mixed $n): Foo { $m = new Maker(); return $m->make($n); }\n\
        $f = outer(1);\nneedString($f->n);\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "the allocation crossed two boundaries: {f:#?}");
    assert_eq!(f[0].id, ID);
}

// ---------------------------------------------------------------------------
// One walk per body: the value position shares the statement position's memo.
// ---------------------------------------------------------------------------

#[test]
fn a_diagnostic_inside_the_callee_is_emitted_once() {
    // The method's body sinks its own argument. Called once in value position, the
    // finding must appear once — a second resolver, or a second `$this` rendering,
    // would walk the body twice under two keys.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        function f(mixed $v): void {}\n\
        final class C { public function m(int $n): mixed { needString($n); return $n; } }\n\
        $c = new C();\nf($c->m(1));\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "one walk, one finding: {f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn the_value_and_statement_positions_key_the_walk_alike() {
    // Same receiver, same argument, two positions. The memo is per descent TREE, and
    // two statements start two trees, so each call walks and reports once — the
    // claim is that value position behaves as statement position does, not that the
    // second call is free. The method pair and the function pair must agree, which
    // is what says the `this:` key rendering added nothing that separates them.
    let method = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        function f(mixed $v): void {}\n\
        final class C { public function m(int $n): mixed { needString($n); return $n; } }\n\
        $c = new C();\n$x = $c->m(1);\nf($c->m(1));\n";
    let function = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        function f(mixed $v): void {}\n\
        function m(int $n): mixed { needString($n); return $n; }\n\
        $x = m(1);\nf(m(1));\n";
    assert_eq!(findings(method).len(), findings(function).len(), "one shape, two spellings");

    // Within ONE statement the memo does bind the two: `f($c->m(1), $c->m(1))` walks
    // the body once, both arguments being resolved under the same tree.
    let twice = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        function f(mixed $a, mixed $b): void {}\n\
        final class C { public function m(int $n): mixed { needString($n); return $n; } }\n\
        $c = new C();\nf($c->m(1), $c->m(1));\n";
    let t = findings(twice);
    assert_eq!(t.len(), 1, "one walk for the two arguments: {t:#?}");
}

// ---------------------------------------------------------------------------
// The declines, each with its own reason.
// ---------------------------------------------------------------------------

#[test]
fn nullsafe_declines_in_both_positions() {
    // ADR-0075 §3.1: `?->` evaluates to `null` when the receiver is, so neither the
    // summary nor the declared arms describe the result.
    let value = format!("{BOXM}$b = new Box(1);\nneedString($b?->unwrap());\n");
    assert_eq!(count(&value), 0, "the value position declines");
    assert_eq!(
        dumped(&format!("{BOXM}$b = new Box(1);\n\\PHPStan\\dumpType($b?->get());")),
        "dumped type: unknown",
    );
    // The statement rung, which used to rebind as if the receiver were non-null —
    // including the DECLARED return type, which `apply_assign`'s own fallback would
    // otherwise re-seed.
    let assigned = "<?php\nclass Box {\n\
        \x20 public function __construct(public mixed $value) {}\n\
        \x20 public function label(): string { return 'x'; }\n\
        }\n\
        $b = new Box(1);\n$v = $b?->label();\n\\PHPStan\\dumpType($v);";
    assert_eq!(dumped(assigned), "dumped type: unknown");
    // The non-nullsafe spelling of the very same call still answers, which is what
    // locates the silence in the `?->` rather than in the resolution.
    assert_eq!(dumped(&assigned.replace("$b?->label()", "$b->label()")), "dumped type: 'x'");
}

#[test]
fn a_prop_receiver_is_still_no_dispatch_target() {
    // ADR-0052 §7: `$a->p->m()` is carried by the IR and declined by the resolver,
    // which is exactly the division the amendment relies on.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        final class Inner { public function m(): mixed { return 1; } }\n\
        final class Outer { public function __construct(public Inner $p) {} }\n\
        $o = new Outer(new Inner());\nneedString($o->p->m());\n";
    assert_eq!(count(src), 0, "a property-fetch receiver resolves to nothing");
}

#[test]
fn an_object_result_is_not_rendered_in_value_position() {
    // ADR-0057 B5's fence, now the only thing holding: the value consumers read
    // `summary.value` and an object has none. The assignment form is the contrast.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        final class Foo { public function __construct(public int $n) {} }\n\
        final class Maker { public function makeFoo(): Foo { return new Foo(1); } }\n\
        $m = new Maker();\n";
    assert_eq!(
        dumped(&format!("{src}\\PHPStan\\dumpType($m->makeFoo()->n);")),
        "dumped type: unknown",
        "a property read off the direct form is not a value either",
    );
    assert_eq!(count(&format!("{src}needString($m->makeFoo()->n);\n")), 0);
    // The rung that DOES answer, for the contrast.
    let assigned = format!("{src}$f = $m->makeFoo();\nneedString($f->n);\n");
    assert_eq!(count(&assigned), 1, "the assignment form rebinds the allocation");
}

#[test]
fn the_store_less_fold_road_sees_no_method() {
    // `resolve_literal_under` has no store in its signature, so the fold and concat
    // lanes decline a method call outright — recorded, not accidental. Since issue
    // #627 the concatenation still answers its own operator fact (`'b'` is
    // non-falsy, so the result is), but the method's `'a'` never becomes a value:
    // `'ab'` is the leak this pins against.
    let src = "<?php\nfinal class C { public function m(): mixed { return 'a'; } }\n\
        $c = new C();\n$s = $c->m() . 'b';\n\\PHPStan\\dumpType($s);";
    assert_eq!(dumped(src), "dumped type: non-falsy-string");
}

#[test]
fn a_named_argument_list_declines_the_binding() {
    // The positional descent cannot map named arguments, exactly as `f(x: 1)` is
    // declined — the value form inherits the statement form's gate.
    let src = "<?php\nfinal class C { public function m(int $n): mixed { return $n; } }\n\
        $c = new C();\n\\PHPStan\\dumpType($c->m(n: 1));";
    assert_eq!(dumped(src), "dumped type: unknown");
}

// ---------------------------------------------------------------------------
// Nested escape and sweep (ADR-0075 §3.2).
// ---------------------------------------------------------------------------

/// A box whose property is written by the callee, so a stale caller-side fact is
/// observable as a finding at a sink after the call.
const MUT: &str = "<?php\ndeclare(strict_types=1);\n\
    function needString(string $s): void {}\n\
    class Box { public function __construct(public mixed $value) {} }\n\
    function mutate(Box $b): mixed { $b->value = 's'; return 1; }\n\
    function outer(mixed $v): void {}\n";

#[test]
fn a_nested_function_call_escapes_and_sweeps_its_argument() {
    // `f(g($b))` handed `$b` to `g` and swept nothing — a pre-existing hole the
    // carrier work made reachable for methods too (ADR-0075 §3.2).
    let src = format!("{MUT}$b = new Box(1);\nouter(mutate($b));\nneedString($b->value);\n");
    assert_eq!(count(&src), 0, "the nested pass swept the stale prop");
    // The top-level form, which always swept — the two must agree.
    let top = format!("{MUT}$b = new Box(1);\nmutate($b);\nneedString($b->value);\n");
    assert_eq!(count(&top), 0);
    // And the sweep is real rather than the fact never having been there.
    let untouched = format!("{MUT}$b = new Box(1);\nneedString($b->value);\n");
    assert_eq!(count(&untouched), 1, "the fact exists until something takes it");
}

#[test]
fn a_nested_method_calls_receiver_escapes_and_sweeps() {
    // `f($b->m())` hands `$b` to `m` as its `$this`; the receiver at a nested
    // position escapes exactly as a top-level one does.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        function outer(mixed $v): void {}\n\
        class Box {\n\
        \x20 public function __construct(public mixed $value) {}\n\
        \x20 public function touch(): mixed { $this->value = 's'; return 1; }\n\
        }\n\
        $b = new Box(1);\nouter($b->touch());\nneedString($b->value);\n";
    assert_eq!(count(src), 0, "the nested receiver was swept");
}

#[test]
fn a_readonly_prop_survives_the_nested_sweep() {
    // The sweep's own boundary, unchanged at depth: `readonly` is a language
    // guarantee, so the fact stands however the object was passed.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        function outer(mixed $v): void {}\n\
        function mutate(Box $b): mixed { return 1; }\n\
        class Box { public function __construct(public readonly mixed $value) {} }\n\
        $b = new Box(1);\nouter(mutate($b));\nneedString($b->value);\n";
    assert_eq!(count(src), 1, "a readonly prop crosses the sweep");
}
