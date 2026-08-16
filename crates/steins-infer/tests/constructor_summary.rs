//! The constructor summary (ADR-0057's 2026-08-16 amendment, issue #385): `new
//! C(args)` is the constructor descent's `$this` snapshot, copied back into the fresh
//! allocation. The descent that already walked `__construct` for its diagnostics now
//! runs with `$this` seeded from the object the site mints — every literal default,
//! every promoted parameter, `escaped = false` — and every exit of that walk
//! contributes the callee's `$this`.
//!
//! Property facts are observed the way `object_state.rs` and `call_site_heap_entry.rs`
//! observe them: through a typed sink (`needString($o->p)`), which fires only if the
//! heap kept the fact, and through `dumpType`. Every fixture declares
//! `strict_types=1` — in coercive mode an `int` reaches a `string` parameter by
//! conversion and no engine reports it, so the coercive surface witnesses nothing.

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
    // `untyped.*` reports on the fixtures' own deliberately-untyped declarations, not
    // the behaviour under test — dropped to keep assertions stable.
    check_project(&db, project, &mut NoFold)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn count(src: &str) -> usize {
    findings(src).len()
}

/// The single dump a fixture asks for.
fn dumped(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one dump, got {ds:?}");
    ds[0].message.clone()
}

/// Every dump a fixture asks for, in emission order.
fn dumps(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

/// Two typed sinks and the flagship class: a property the constructor's **body**
/// writes, which the declaration alone can say nothing about.
const P: &str = "<?php\ndeclare(strict_types=1);\n\
    function needInt(int $x): void {}\n\
    function needString(string $s): void {}\n\
    class B { public $value; public function __construct($v) { $this->value = $v; } }\n";

// ---------------------------------------------------------------------------
// The flagship: a body-written property, at each of the three positions the
// amendment's C7 covers.
// ---------------------------------------------------------------------------

#[test]
fn a_body_written_property_reaches_the_assignment_form() {
    let src = format!("{P}$b = new B(1);\nneedString($b->value);\n");
    let f = findings(&src);
    assert_eq!(f.len(), 1, "the constructor's own write premises the sink: {f:#?}");
    assert_eq!(f[0].id, ID);

    assert_eq!(dumped(&format!("{P}$b = new B(1);\n\\PHPStan\\dumpType($b->value);\n")), "dumped type: 1");
}

#[test]
fn a_body_written_property_crosses_into_argument_position() {
    // ADR-0086's argument leg reads the object the `new` mints, and that object is
    // now the snapshot — so the sink INSIDE the callee fires. This is the one
    // position where the walk runs at the mint (C7): no `Callee::Construct` call is
    // lowered for a `new` in argument position at all.
    let src = format!(
        "{P}function takes(B $b): void {{ needString($b->value); }}\ntakes(new B(1));\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);

    // And the same object read back out through the return channel (#378).
    assert_eq!(
        dumped(&format!(
            "{P}function get(B $b) {{ return $b->value; }}\n$v = get(new B(1));\n\\PHPStan\\dumpType($v);\n"
        )),
        "dumped type: 1",
    );
}

#[test]
fn a_body_written_property_composes_with_the_378_factory() {
    // `return new B(1)` snapshots the CONSTRUCTED state, so two boundaries compose:
    // the constructor's write, then the factory's own heap summary.
    let src = format!("{P}function mk(int $n): B {{ return new B($n); }}\n$b = mk(1);\n");
    assert_eq!(
        dumped(&format!("{src}\\PHPStan\\dumpType($b->value);\n")),
        "dumped type: 1",
    );
    let f = findings(&format!("{src}needString($b->value);\n"));
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

// ---------------------------------------------------------------------------
// Defaults: the walk supersedes the ADR-0086 §4 lexical gate where it runs.
// ---------------------------------------------------------------------------

#[test]
fn an_untouched_default_stands_and_a_written_one_reads_the_body() {
    // `kept` is never written, so its default IS the constructed value; `view` is
    // written, so the body's value is what the caller reads — never the default, and
    // no longer merely unknown as the lexical floor left it.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class D { public $view = 0; public $kept = 3;\n\
        \x20 public function __construct(int $v) { $this->view = $v; } }\n\
        $d = new D(5);\n\\PHPStan\\dumpType($d->view);\n\\PHPStan\\dumpType($d->kept);\n";
    assert_eq!(dumps(src), vec!["dumped type: 5", "dumped type: 3"]);
}

#[test]
fn a_dynamic_this_write_keeps_no_stale_default() {
    // `$this->$k = …` lowers to a `Barrier`, which clears the store, so `$this` is
    // gone at the fall-through and the exit contributes the value floor — the heap
    // summary dies and the site takes the ADR-0086 §4 floor, which for a dynamic
    // access drops every default (C2/C6). Whichever way it lands, it must not be `0`.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class Dy { public $view = 0;\n\
        \x20 public function __construct(string $k, int $v) { $this->$k = $v; } }\n\
        $d = new Dy('view', 5);\n\\PHPStan\\dumpType($d->view);\n";
    assert_eq!(dumped(src), "dumped type: unknown");
}

#[test]
fn the_private_shape_guard_never_reads_a_default_as_proven() {
    // The owner-probe shape ADR-0086 §4 was amended for, now with the walk on: the
    // constructor computes `view` from an unknown, so `view` is unknown after `new`
    // — never the declared `0` — and the getter that returns it premises nothing
    // against a declared `positive-int`.
    let cls = "<?php\ndeclare(strict_types=1);\n\
        class H { private int $view = 0; private int $ad_count = 0;\n\
        \x20 public function __construct(int $original) { $this->view = $original - $this->ad_count; }\n\
        \x20 public function getView(): int { return $this->view; } }\n\
        /** @param positive-int $n */\nfunction perPage(int $t, $n): int { return 1; }\n";
    assert_eq!(
        dumped(&format!("{cls}function run(int $o): void {{ $h = new H($o);\n\\PHPStan\\dumpType($h->view); }}\n")),
        "dumped type: unknown",
    );
    assert_eq!(
        count(&format!(
            "{cls}function run(int $o): void {{ $h = new H($o); $v = $h->getView(); perPage(10, $v); }}\n"
        )),
        0,
        "a computed slot convicts nobody",
    );
}

// ---------------------------------------------------------------------------
// Delegation and leaks: what an unescaped `$this` still owes (C5).
// ---------------------------------------------------------------------------

#[test]
fn a_delegating_constructor_yields_the_delegates_own_write() {
    // MOVED 2026-08-17 (issue #420), deliberately: this pin used to read `unknown`,
    // because a same-`$this` call swept. It descends and copies back now (ADR-0057's
    // same-`$this` amendment, D1/D4) — `init`'s `$this` is a copy of the walk's own,
    // unescaped inside a constructor walk, so its write is the object's. Still
    // emphatically not the declared default: the value is the delegate's `2`, and a
    // delegate that wrote nothing would leave the slot swept, not defaulted.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class Del { public $value = 0;\n\
        \x20 public function __construct() { $this->init(); }\n\
        \x20 private function init(): void { $this->value = 2; } }\n\
        $d = new Del();\n\\PHPStan\\dumpType($d->value);\n";
    assert_eq!(dumped(src), "dumped type: 2");

    // Write AFTER the delegation and the write stands: the sweep is a statement
    // effect, not a verdict on the whole body.
    let after = "<?php\ndeclare(strict_types=1);\n\
        class Del2 { public $value = 0;\n\
        \x20 public function __construct() { $this->init(); $this->value = 7; }\n\
        \x20 private function init(): void {} }\n\
        $d = new Del2();\n\\PHPStan\\dumpType($d->value);\n";
    assert_eq!(dumped(after), "dumped type: 7");
}

#[test]
fn a_constructor_that_delegates_by_the_class_own_name_descends() {
    // MOVED with the test above (#420): `Foo::init()` in `Foo`'s own constructor is
    // `self::init()` under another spelling (issue #417), so it takes the same road —
    // the same seed, the same copy-back — and not merely the same sweep.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class Del3 { public $value = 0;\n\
        \x20 public function __construct() { Del3::init(); }\n\
        \x20 private function init(): void { $this->value = 2; } }\n\
        $d = new Del3();\n\\PHPStan\\dumpType($d->value);\n";
    assert_eq!(dumped(src), "dumped type: 2");
}

#[test]
fn a_constructor_that_delegates_to_an_ancestor_by_name_descends() {
    // The named class need only be an ancestor of the class whose constructor is
    // running — `DBase::init()` written inside `DChild`'s own constructor still runs
    // with `DChild`'s `$this` (PHP forwards it: `$this` is-a `DBase`) — and the seed
    // is that `$this`, class `DChild`, so `DBase::init`'s write lands on the child's
    // own slot (#420).
    let src = "<?php\ndeclare(strict_types=1);\n\
        class DBase { public $value = 0; public function init(): void { $this->value = 2; } }\n\
        class DChild extends DBase {\n\
        \x20 public function __construct() { DBase::init(); } }\n\
        $d = new DChild();\n\\PHPStan\\dumpType($d->value);\n";
    assert_eq!(dumped(src), "dumped type: 2");
}

#[test]
fn a_constructor_that_calls_a_static_method_by_name_does_not_sweep() {
    // A resolved STATIC method never carries `$this` (issue #417's other half): the
    // constructor's own write stands.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class Del4 { public $value = 0;\n\
        \x20 public function __construct() { $this->value = 2; Del4::helper(); }\n\
        \x20 public static function helper(): void {} }\n\
        $d = new Del4();\n\\PHPStan\\dumpType($d->value);\n";
    assert_eq!(dumped(src), "dumped type: 2");
}

#[test]
fn a_constructor_that_calls_an_unrelated_class_by_name_does_not_sweep() {
    // A class unrelated to the one whose constructor is running carries no `$this`
    // either — the write stands, the precision payoff the ancestor check buys.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class DOther { public function m(): void {} }\n\
        class Del5 { public $value = 0;\n\
        \x20 public function __construct() { $this->value = 2; DOther::m(); } }\n\
        $d = new Del5();\n\\PHPStan\\dumpType($d->value);\n";
    assert_eq!(dumped(src), "dumped type: 2");
}

#[test]
fn a_constructor_that_leaks_this_yields_a_pre_escaped_swept_object() {
    // `register($this)` escapes the allocation inside the walk, and the sweep that
    // rides the same statement takes its non-readonly props with it. The `escaped`
    // bit ORs into the snapshot (§2.4), so the caller's object is born pre-escaped
    // and its next unknown call sweeps it as one it leaked itself.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function register(object $o): void {}\n\
        class Leak { public $value = 0; public readonly string $tag;\n\
        \x20 public function __construct() { $this->tag = 'x'; register($this); } }\n\
        $l = new Leak();\n\\PHPStan\\dumpType($l->value);\n\\PHPStan\\dumpType($l->tag);\n";
    // The non-readonly default is swept inside the walk; the readonly one is immune
    // by the language guarantee ADR-0036 records, and crosses.
    assert_eq!(dumps(src), vec!["dumped type: unknown", "dumped type: 'x'"]);
}

// ---------------------------------------------------------------------------
// Exits: the join, and the paths that yield nothing.
// ---------------------------------------------------------------------------

#[test]
fn a_multi_exit_constructor_joins_its_paths() {
    // Two exits — the `return;` in the branch and the body's fall-through — and the
    // join is `join_heap_exits` verbatim (C3).
    let src = "<?php\ndeclare(strict_types=1);\n\
        class M { public $a;\n\
        \x20 public function __construct(bool $f) { if ($f) { $this->a = 1; return; } $this->a = 2; } }\n\
        function build(bool $f): void { $m = new M($f);\n\\PHPStan\\dumpType($m->a); }\n";
    assert_eq!(dumped(src), "dumped type: 1|2");
}

#[test]
fn a_property_set_on_one_path_only_does_not_survive() {
    let src = "<?php\ndeclare(strict_types=1);\n\
        class One { public $a; public $b;\n\
        \x20 public function __construct(bool $f) { $this->a = 1; if ($f) { $this->b = 2; return; } } }\n\
        function build(bool $f): void { $o = new One($f);\n\
        \\PHPStan\\dumpType($o->a);\n\\PHPStan\\dumpType($o->b); }\n";
    assert_eq!(dumps(src), vec!["dumped type: 1", "dumped type: unknown"]);
}

#[test]
fn an_always_throwing_constructor_keeps_todays_object() {
    // No exit yields the object, so there is no summary at all and the site keeps
    // what `new_heap_object` builds under the lexical gate (C2/C6) — here, the
    // untouched default, since the body names no slot.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class T { public $d = 3;\n\
        \x20 public function __construct() { throw new \\RuntimeException('no'); } }\n\
        $t = new T();\n\\PHPStan\\dumpType($t->d);\n";
    assert_eq!(dumped(src), "dumped type: 3");
}

#[test]
fn a_throw_on_one_path_leaves_the_other_paths_state() {
    // A `throw` is not an exit, so the surviving path answers alone.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class T2 { public $a = 0;\n\
        \x20 public function __construct(bool $f) { if ($f) { throw new \\RuntimeException('no'); } $this->a = 9; } }\n\
        function build(bool $f): void { $t = new T2($f);\n\\PHPStan\\dumpType($t->a); }\n";
    assert_eq!(dumped(src), "dumped type: 9");
}

// ---------------------------------------------------------------------------
// Inheritance.
// ---------------------------------------------------------------------------

#[test]
fn an_inherited_constructor_writes_the_childs_object() {
    // `resolve_exact` runs the chain's constructor, and `$this` is seeded exactly the
    // CHILD (the allocation's own class), so a `Base::__construct` write lands on the
    // child object.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class Base { public $x; public function __construct(int $v) { $this->x = $v; } }\n\
        class Child extends Base { public $own = 1; }\n\
        $c = new Child(4);\n\\PHPStan\\dumpType($c->x);\n\\PHPStan\\dumpType($c->own);\n";
    assert_eq!(dumps(src), vec!["dumped type: 4", "dumped type: 1"]);
}

#[test]
fn a_parent_construct_chain_descends_and_copies_back() {
    // MOVED 2026-08-17 (#420): `parent::__construct()` runs with the SAME `$this`
    // under another spelling, so it descends and copies back like every other
    // same-`$this` call. The parent's write is the child object's, and the child's own
    // write after the chain call still stands.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class PBase { public $x = 0; public function __construct(int $v) { $this->x = $v; } }\n\
        class PChild extends PBase { public $own;\n\
        \x20 public function __construct(int $v) { parent::__construct($v); $this->own = 5; } }\n\
        $c = new PChild(4);\n\\PHPStan\\dumpType($c->x);\n\\PHPStan\\dumpType($c->own);\n";
    assert_eq!(dumps(src), vec!["dumped type: 4", "dumped type: 5"]);
}

// ---------------------------------------------------------------------------
// The decline floor (C6).
// ---------------------------------------------------------------------------

#[test]
fn a_poisoned_constructor_declines_to_the_lexical_floor() {
    // `extract` poisons the callee scope, so the descent refuses at its first line
    // and the lexical gate answers — and its answer for a poisoned constructor is
    // that NO default survives (ADR-0086 §4: a slot can be written without being
    // spelled).
    let src = "<?php\ndeclare(strict_types=1);\n\
        class Px { public $a = 3;\n\
        \x20 public function __construct(array $vars) { extract($vars); } }\n\
        $p = new Px([]);\n\\PHPStan\\dumpType($p->a);\n";
    assert_eq!(dumped(src), "dumped type: unknown");
}

#[test]
fn a_recursive_constructor_terminates_on_the_stack_guard() {
    // The inner `new Rec($n)` reaches the same binding key, which is already on the
    // descent stack: no summary for it, the floor for its site, and the outer walk
    // carries on and snapshots its own write.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class Rec { public $v = 0;\n\
        \x20 public function __construct(int $n) { $x = new Rec($n); $this->v = 1; } }\n\
        $r = new Rec(1);\n\\PHPStan\\dumpType($r->v);\n";
    assert_eq!(dumped(src), "dumped type: 1");
}

#[test]
fn a_named_argument_list_declines() {
    // The descent is positional-only (§3), so `new C(x: 1)` declines exactly as
    // `f(x: 1)` does and the lexical floor answers — which here drops the default the
    // body mentions, leaving the slot unknown rather than the body's `1`.
    let named = "<?php\ndeclare(strict_types=1);\n\
        class N { public $a = 0; public function __construct(int $x) { $this->a = $x; } }\n\
        $n = new N(x: 1);\n\\PHPStan\\dumpType($n->a);\n";
    assert_eq!(dumped(named), "dumped type: unknown");

    // The positional twin, for the contrast.
    let positional = named.replace("new N(x: 1)", "new N(1)");
    assert_eq!(dumped(&positional), "dumped type: 1");
}

// ---------------------------------------------------------------------------
// One walk per site, and the memo key (C7/C8).
// ---------------------------------------------------------------------------

#[test]
fn a_diagnostic_inside_the_constructor_is_emitted_once_per_entry_state() {
    // `needString($v)` fires only under a bound argument, so it is a descent-only
    // finding and counts the walks that reached it.
    let cls = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class E { public function __construct(int $v) { needString($v); } }\n";

    let once = format!("{cls}$a = new E(1);\n");
    assert_eq!(count(&once), 1, "one site, one finding");

    // Two sites with the same entry state ON ONE LINE are one finding: one key, one
    // provenance, one emission — the `leg5` pin of `call_site_heap_entry.rs` in its
    // constructor spelling.
    let one_line = format!("{cls}$a = new E(1); $b = new E(1);\n");
    assert_eq!(count(&one_line), 1, "one key, one emission");

    // On two lines they are two, and this is the provenance surface, not a second
    // walk of a shared key: the finding names the site that bound the argument
    // ("bound at new E(1) call on line N"), so the two messages differ and the
    // run-level dedupe has nothing to collapse.
    let two_lines = format!("{cls}$a = new E(1);\n$b = new E(1);\n");
    let f = findings(&two_lines);
    assert_eq!(f.len(), 2, "{f:#?}");
    assert!(f[0].message != f[1].message, "each names its own site");

    // Two DIFFERENT entry states are two judgments — the binding key names the
    // arguments, so neither replays the other's summary nor suppresses its emission.
    let differing = format!("{cls}$a = new E(1); $b = new E(2);\n");
    assert_eq!(count(&differing), 2, "different entry states, both judged");
}

#[test]
fn the_key_names_the_seeded_this_so_defaults_do_not_replay() {
    // Two classes sharing one inherited constructor body, with different literal
    // defaults on the slot the body leaves alone. The `this:` component carries the
    // seeded object's canonical rendering (C8), so neither answers for the other.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class KBase { public $kept; public $w; public function __construct(int $v) { $this->w = $v; } }\n\
        class K1 extends KBase { public $kept = 1; }\n\
        class K2 extends KBase { public $kept = 2; }\n\
        $a = new K1(0);\n$b = new K2(0);\n\
        \\PHPStan\\dumpType($a->kept);\n\\PHPStan\\dumpType($b->kept);\n";
    assert_eq!(dumps(src), vec!["dumped type: 1", "dumped type: 2"]);
}

// ---------------------------------------------------------------------------
// Soundness legs that ride along.
// ---------------------------------------------------------------------------

#[test]
fn exactness_and_readonly_bookkeeping_cross_unchanged() {
    // A readonly slot written by the constructor's own body is recorded written, so a
    // caller-side second write is the proven `readonly.reassigned` it always was, and
    // the slot is sweep-immune.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function unknownFn(): void {}\n\
        class Ro { public readonly string $name;\n\
        \x20 public function __construct() { $this->name = 'abc'; } }\n\
        $r = new Ro();\nunknownFn();\n\\PHPStan\\dumpType($r->name);\n";
    assert_eq!(dumped(src), "dumped type: 'abc'");
}

#[test]
fn a_hooked_property_binds_no_fact_through_the_walk_either() {
    // FP class 16: a hooked property's write runs arbitrary code, so the stored value
    // is not the written one. The exclusion is inherited by construction — a hooked
    // slot never enters the heap, so it cannot enter the snapshot.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class Hk { public string $v { get => 'x'; set => $value; }\n\
        \x20 public function __construct() { $this->v = 'abc'; } }\n\
        $h = new Hk();\n\\PHPStan\\dumpType($h->v);\n";
    assert_eq!(dumped(src), "dumped type: unknown");
}
