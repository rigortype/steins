//! A same-`$this` call descends and copies `$this` back (ADR-0057's 2026-08-17
//! amendment, issue #420). C5's sweep — the receiver's own non-readonly props and
//! carries dropped at `$this->m()`, `parent::__construct()`, `self::m()` and the rest
//! — becomes the **decline floor**; where the target resolves, the callee's `$this` is
//! a copy of the caller's, its exits are snapshotted, and the join is copied back.
//!
//! The receiver leg travels with it (D2): an exact `$o->m()` already seeded its
//! `$this` from a copy of `$o` (ADR-0086 §3), so its snapshot is copied back into `$o`
//! and a fluent setter reads its own write.
//!
//! Property facts are observed as `constructor_summary.rs` observes them: through a
//! typed sink (`needString($o->p)`), which fires only if the heap kept the fact, and
//! through `dumpType`. Every fixture declares `strict_types=1` — in coercive mode an
//! `int` reaches a `string` parameter by conversion and no engine reports it, so the
//! coercive surface witnesses nothing.

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

const HEAD: &str = "<?php\ndeclare(strict_types=1);\n\
    function needInt(int $x): void {}\n\
    function needString(string $s): void {}\n";

// ---------------------------------------------------------------------------
// The flagship: a delegating constructor (D1/D4).
// ---------------------------------------------------------------------------

#[test]
fn a_delegating_constructor_carries_the_delegates_write_to_the_proof_layer() {
    // The amendment's own D8 shape. Inside a constructor walk `$this` is unescaped
    // (C1), so its non-readonly props cross into `init`'s copy, `init`'s write lands
    // there, and the joined exit snapshot replaces the caller's `$this`.
    let cls = "class B { public $value;\n\
        \x20 public function __construct($v) { $this->init($v); }\n\
        \x20 private function init($v): void { $this->value = $v; } }\n";
    let src = format!("{HEAD}{cls}$b = new B(2);\n\\PHPStan\\dumpType($b->value);\n");
    assert_eq!(dumped(&src), "dumped type: 2");

    // …and it is a premise, not merely a dump.
    let sink = format!("{HEAD}{cls}$b = new B(2);\nneedString($b->value);\n");
    let f = findings(&sink);
    assert_eq!(f.len(), 1, "the delegate's own write premises the sink: {f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn a_delegation_chain_recurses_under_the_budget() {
    // Depth, pinned: the descent is the ordinary binding descent, so a chain of
    // delegations recurses under `MAX_BINDING_DEPTH` rather than stopping at one
    // level. Three frames deep here, and the innermost write is the object's.
    let src = format!(
        "{HEAD}class C3 {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->a(); }}\n\
        \x20 private function a(): void {{ $this->b(); }}\n\
        \x20 private function b(): void {{ $this->value = 7; }} }}\n\
        $c = new C3();\n\\PHPStan\\dumpType($c->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: 7");
}

#[test]
fn a_write_before_the_delegation_survives_a_delegate_that_leaves_it_alone() {
    // The copy carries the caller's own writes IN (the unescaped constructor `$this`),
    // so a slot the delegate never touches comes back with what was already there —
    // the sweep used to drop it.
    let src = format!(
        "{HEAD}class Keep {{ public $a; public $b;\n\
        \x20 public function __construct() {{ $this->a = 1; $this->init(); }}\n\
        \x20 private function init(): void {{ $this->b = 2; }} }}\n\
        $k = new Keep();\n\\PHPStan\\dumpType($k->a);\n\\PHPStan\\dumpType($k->b);\n"
    );
    assert_eq!(dumps(&src), vec!["dumped type: 1", "dumped type: 2"]);
}

// ---------------------------------------------------------------------------
// `parent::__construct()` and its chain (D1).
// ---------------------------------------------------------------------------

#[test]
fn a_parent_construct_writes_the_childs_object() {
    // `parent::__construct()` runs with the same `$this` under another spelling, and
    // the seed is the CHILD's allocation, so the parent's write lands on the child's
    // own slot. The child's later write stands beside it.
    let src = format!(
        "{HEAD}class PB {{ public $x = 0; public function __construct(int $v) {{ $this->x = $v; }} }}\n\
        class PC extends PB {{ public $own;\n\
        \x20 public function __construct(int $v) {{ parent::__construct($v); $this->own = 5; }} }}\n\
        $c = new PC(4);\n\\PHPStan\\dumpType($c->x);\n\\PHPStan\\dumpType($c->own);\n"
    );
    assert_eq!(dumps(&src), vec!["dumped type: 4", "dumped type: 5"]);
}

#[test]
fn a_grandparent_chain_recurses_rather_than_stopping_at_one_level() {
    // The depth question, answered: `parent::__construct()` inside the parent is the
    // same mechanism one frame down, so the GRANDparent's write reaches the child's
    // object too — recursion under the budget, not a single level. Each generation's
    // own write is pinned so the chain is visible rather than merely summed.
    let src = format!(
        "{HEAD}class G1 {{ public $a = 0; public function __construct() {{ $this->a = 1; }} }}\n\
        class G2 extends G1 {{ public $b = 0;\n\
        \x20 public function __construct() {{ parent::__construct(); $this->b = 2; }} }}\n\
        class G3 extends G2 {{ public $c = 0;\n\
        \x20 public function __construct() {{ parent::__construct(); $this->c = 3; }} }}\n\
        $g = new G3();\n\
        \\PHPStan\\dumpType($g->a);\n\\PHPStan\\dumpType($g->b);\n\\PHPStan\\dumpType($g->c);\n"
    );
    assert_eq!(dumps(&src), vec!["dumped type: 1", "dumped type: 2", "dumped type: 3"]);
}

// ---------------------------------------------------------------------------
// The receiver leg (D2): a fluent setter reads its own write.
// ---------------------------------------------------------------------------

#[test]
fn a_fluent_setter_reads_its_own_write() {
    // `$o->setX(1)` is NOT a same-`$this` call — it is a receiver call on `$o` — and
    // it takes the same road: ADR-0086 §3 already seeded `setX`'s `$this` from a copy
    // of `$o`, and D2 reads the snapshot back into `$o`, replacing the #295/#377
    // caller-side sweep with the walk's own truth.
    let cls = "final class F { public $x = 0;\n\
        \x20 public function setX(int $v): self { $this->x = $v; return $this; } }\n";
    let src = format!("{HEAD}{cls}$o = new F();\n$o->setX(1);\n\\PHPStan\\dumpType($o->x);\n");
    assert_eq!(dumped(&src), "dumped type: 1");

    // And as a premise.
    let sink = format!("{HEAD}{cls}$o = new F();\n$o->setX(1);\nneedString($o->x);\n");
    let f = findings(&sink);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn a_setter_that_writes_nothing_leaves_the_receivers_slot_alone() {
    // The copy-back replaces rather than merges, and the replacement is the walk's
    // truth: a method that touches nothing hands back what it was given, so a slot
    // proven before the call is still proven after it.
    let src = format!(
        "{HEAD}final class N {{ public $x = 0;\n\
        \x20 public function touch(): void {{}} }}\n\
        $o = new N();\n$o->x = 4;\n$o->touch();\n\\PHPStan\\dumpType($o->x);\n"
    );
    assert_eq!(dumped(&src), "dumped type: 4");
}

#[test]
fn a_this_method_call_inside_an_ordinary_method_crosses_only_readonly_props() {
    // D1's second bullet, pinned. Outside a constructor the walk's `$this` is
    // pre-escaped (`seed_this_object`), so the copy takes no non-readonly prop — the
    // caller's `$this->v = 1` is invisible to the delegate, whose own read is
    // therefore unknown and premises nothing. What DOES come back is the delegate's
    // own write, which is the half this slice adds.
    let src = format!(
        "{HEAD}final class E {{ public $v = 0; public $w = 0;\n\
        \x20 public function run(): void {{ $this->v = 1; $this->fill(); \\PHPStan\\dumpType($this->w); }}\n\
        \x20 private function fill(): void {{ needString($this->v); $this->w = 9; }} }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: 9");
    assert_eq!(
        findings(&src).into_iter().filter(|d| d.id == ID).count(),
        0,
        "a pre-escaped `$this` hands over no non-readonly prop, so the delegate's read convicts nobody",
    );
}

// ---------------------------------------------------------------------------
// Leaks: what comes back when the callee lets `$this` out (D4).
// ---------------------------------------------------------------------------

#[test]
fn a_leak_inside_the_callee_comes_back_pre_escaped_and_swept() {
    // `register($this)` inside the delegate escapes the copy and sweeps its
    // non-readonly props (the callee's own ADR-0036 discipline, running on the copy),
    // and `escaped` crosses back with the snapshot. So the write before the leak does
    // not survive it, and the caller's object is pre-escaped afterwards.
    let src = format!(
        "{HEAD}function register($o): void {{}}\n\
        class L {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->init(); }}\n\
        \x20 private function init(): void {{ $this->value = 2; register($this); }} }}\n\
        $l = new L();\n\\PHPStan\\dumpType($l->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");

    // The pre-escape is observable, and it crosses TWO boundaries to get here: the
    // delegate leaked its copy, `escaped` came back on the snapshot, the copy-back
    // put it on the constructor's `$this`, and C4 put that on the caller's object. A
    // later unresolvable call then sweeps a write the same object would have kept.
    let escaped = format!(
        "{HEAD}function register($o): void {{}}\n\
        class L2 {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->init(); }}\n\
        \x20 private function init(): void {{ register($this); }} }}\n\
        $l = new L2();\n$l->value = 3;\nunknownFn();\n\\PHPStan\\dumpType($l->value);\n"
    );
    assert_eq!(dumped(&escaped), "dumped type: unknown");

    // …where nothing got out, the same shape survives the same unknown call: the
    // delegate's `escaped = false` is what comes back, so the ADR-0036 payoff is not
    // lost merely by delegating.
    let local = format!(
        "{HEAD}class L3 {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->init(); }}\n\
        \x20 private function init(): void {{}} }}\n\
        $l = new L3();\n$l->value = 3;\nunknownFn();\n\\PHPStan\\dumpType($l->value);\n"
    );
    assert_eq!(dumped(&local), "dumped type: 3");
}

// ---------------------------------------------------------------------------
// The decline floor (D5): each of these sweeps exactly as before.
// ---------------------------------------------------------------------------

#[test]
fn an_unresolvable_target_sweeps() {
    // No such method anywhere in the chain: `resolve_call_target` declines, so there
    // is no descent, no snapshot, and the C5 sweep is the whole answer.
    let src = format!(
        "{HEAD}class U {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->value = 2; $this->missing(); }} }}\n\
        $u = new U();\n\\PHPStan\\dumpType($u->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
}

#[test]
fn a_guarded_refused_target_sweeps() {
    // A non-exact `$this` calling an overridable method: `resolve_guarded` refuses (a
    // subclass may override it), so there is no descent and the sweep is the answer —
    // the delegate's own write reaches nobody. The enclosing method is reached by the
    // plain per-scope pass, where `$this` is a lower bound.
    let src = format!(
        "{HEAD}class Gu {{ public $v = 0;\n\
        \x20 public function run(): void {{ $this->hook(); \\PHPStan\\dumpType($this->v); }}\n\
        \x20 public function hook(): void {{ $this->v = 9; }} }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");

    // `final` on the method lifts the refusal, and then the walk answers. (The
    // caller's OWN pre-call write reaches the delegate in neither shape: outside a
    // constructor `$this` is pre-escaped and hands over no non-readonly prop — D1.)
    let sealed = format!(
        "{HEAD}class Gs {{ public $v = 0;\n\
        \x20 public function run(): void {{ $this->hook(); \\PHPStan\\dumpType($this->v); }}\n\
        \x20 final public function hook(): void {{ $this->v = 9; }} }}\n"
    );
    assert_eq!(dumped(&sealed), "dumped type: 9");
}

#[test]
fn a_poisoned_callee_sweeps() {
    // `extract` poisons the delegate's scope, so `descend` refuses at its first line.
    let src = format!(
        "{HEAD}class Po {{ public $value = 0;\n\
        \x20 public function __construct(array $vars) {{ $this->value = 2; $this->init($vars); }}\n\
        \x20 private function init(array $vars): void {{ extract($vars); }} }}\n\
        $p = new Po([]);\n\\PHPStan\\dumpType($p->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
}

#[test]
fn a_recursion_pair_sweeps_rather_than_looping() {
    // The delegate calls back into its own caller under the same key: the key is on
    // the descent stack, no summary comes back, and the floor stands at that call.
    // Terminating, and silent rather than stale — the write BEFORE the recursive call
    // is what the sweep takes.
    let src = format!(
        "{HEAD}class Re {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->a(); }}\n\
        \x20 private function a(): void {{ $this->b(); }}\n\
        \x20 private function b(): void {{ $this->value = 2; $this->a(); }} }}\n\
        $r = new Re();\n\\PHPStan\\dumpType($r->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");

    // …and a write after it stands, the sweep being a statement effect rather than a
    // verdict on the body.
    let after = format!(
        "{HEAD}class Re2 {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->a(); }}\n\
        \x20 private function a(): void {{ $this->b(); }}\n\
        \x20 private function b(): void {{ $this->a(); $this->value = 2; }} }}\n\
        $r = new Re2();\n\\PHPStan\\dumpType($r->value);\n"
    );
    assert_eq!(dumped(&after), "dumped type: 2");
}

#[test]
fn a_named_argument_list_sweeps() {
    // The binding descent is positional-only (§3), so `$this->init(v: 2)` declines
    // exactly as `f(x: 1)` does — and its positional twin does not.
    let named = format!(
        "{HEAD}class Na {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->init(v: 2); }}\n\
        \x20 private function init(int $v): void {{ $this->value = $v; }} }}\n\
        $n = new Na();\n\\PHPStan\\dumpType($n->value);\n"
    );
    assert_eq!(dumped(&named), "dumped type: unknown");

    let positional = format!(
        "{HEAD}class Np {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->init(2); }}\n\
        \x20 private function init(int $v): void {{ $this->value = $v; }} }}\n\
        $n = new Np();\n\\PHPStan\\dumpType($n->value);\n"
    );
    assert_eq!(dumped(&positional), "dumped type: 2");
}

#[test]
fn a_static_target_carries_no_this_and_needs_no_copy_back() {
    // A resolved STATIC method is the one admitted spelling proven not to carry
    // `$this` (issue #417's other half): it seeds nothing, copies nothing back, and
    // does not sweep either — the constructor's own write simply stands.
    let src = format!(
        "{HEAD}class St {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->value = 2; St::helper(); }}\n\
        \x20 public static function helper(): void {{}} }}\n\
        $s = new St();\n\\PHPStan\\dumpType($s->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: 2");
}

#[test]
fn a_late_static_binding_call_sweeps() {
    // `static::` resolves to nothing at all (`resolve_call_target` has no arm for it),
    // so the sweep is the whole answer and no snapshot can exist.
    let src = format!(
        "{HEAD}class Ls {{ public $value = 0;\n\
        \x20 public function __construct() {{ $this->value = 2; static::hook(); }}\n\
        \x20 public static function hook(): void {{}} }}\n\
        $l = new Ls();\n\\PHPStan\\dumpType($l->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
}

#[test]
fn a_hidden_exit_inside_an_opaque_construct_sweeps() {
    // An `Opaque` that `may_return` hides an exit this walk never sees, so it
    // contributes the floor on the `$this` channel too and the component dies (D3).
    let src = format!(
        "{HEAD}class Op {{ public $value = 0;\n\
        \x20 public function __construct(array $xs) {{ $this->init($xs); }}\n\
        \x20 private function init(array $xs): void {{ $this->value = 2; foreach ($xs as $x) {{ return; }} }} }}\n\
        $o = new Op([]);\n\\PHPStan\\dumpType($o->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
}

#[test]
fn an_unresolved_call_beside_the_copy_back_declines_the_whole_statement() {
    // D4's first guard: the two effects cannot be ordered, and the unknown call may
    // reach `$this` through a closure alias, so the floor stands for the statement.
    let src = format!(
        "{HEAD}class Ec {{ public $value = 0;\n\
        \x20 public function __construct() {{ echo $this->init(), $this->missing(); }}\n\
        \x20 private function init(): string {{ $this->value = 2; return 'a'; }} }}\n\
        $e = new Ec();\n\\PHPStan\\dumpType($e->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
}

#[test]
fn two_snapshots_for_one_name_decline_each_other() {
    // D4's second guard: both descents were seeded from the same pre-statement
    // `$this`, so `b`'s snapshot does not contain `a`'s write, and installing it would
    // erase a write that really happened. Neither is applied.
    let src = format!(
        "{HEAD}class Tw {{ public $a = 0; public $b = 0;\n\
        \x20 public function __construct() {{ echo $this->setA(), $this->setB(); }}\n\
        \x20 private function setA(): string {{ $this->a = 1; return 'x'; }}\n\
        \x20 private function setB(): string {{ $this->b = 2; return 'y'; }} }}\n\
        $t = new Tw();\n\\PHPStan\\dumpType($t->a);\n\\PHPStan\\dumpType($t->b);\n"
    );
    assert_eq!(dumps(&src), vec!["dumped type: unknown", "dumped type: unknown"]);
}

// ---------------------------------------------------------------------------
// The return value is untouched, and value position stays on the floor (D3/D6).
// ---------------------------------------------------------------------------

#[test]
fn the_return_value_of_a_same_this_call_still_rides_its_own_rung() {
    // The `$this` snapshot is a THIRD component (D3), so a same-`$this` call's value
    // summary is exactly what it was: `$this->answer()` still binds `41`.
    let src = format!(
        "{HEAD}final class Rv {{ public $v = 0;\n\
        \x20 public function run(): void {{ $n = $this->answer(); \\PHPStan\\dumpType($n); }}\n\
        \x20 private function answer(): int {{ return 41; }} }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: 41");
}

#[test]
fn a_same_this_call_in_value_position_stays_on_the_sweep_floor() {
    // D6, pinned as a limit rather than a gap: the value-position road holds the
    // caller's store by shared reference and has no write-back channel, so the call
    // sweeps and the props are silent — never stale. The RETURN value crosses there
    // as it always did, which is what makes the silence attributable to the missing
    // channel.
    let src = format!(
        "{HEAD}final class Vp {{ public $v = 0;\n\
        \x20 public function run(): void {{ needInt($this->fill()); \\PHPStan\\dumpType($this->v); }}\n\
        \x20 private function fill(): int {{ $this->v = 5; return 1; }} }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
}

// ---------------------------------------------------------------------------
// One walk per call: the memo, the key, and the emission (D6).
// ---------------------------------------------------------------------------

#[test]
fn a_diagnostic_inside_the_callee_is_emitted_once_per_entry_state() {
    // The memo suppresses the re-walk under an identical key and the run-level dedupe
    // collapses identical findings: two `new K(1)` sites ON ONE LINE — one key, one
    // provenance ("bound at … line N") — report once. Two lines would be two messages
    // and nothing for the dedupe to collapse, which is the provenance surface rather
    // than a second walk (`constructor_summary.rs` pins that half).
    let cls = "class K { public $value = 0;\n\
        \x20 public function __construct(int $v) { $this->init($v); }\n\
        \x20 private function init(int $v): void { needString($v); $this->value = $v; } }\n";
    let twice = format!("{HEAD}{cls}$a = new K(1); $b = new K(1);\n");
    assert_eq!(
        findings(&twice).into_iter().filter(|d| d.id == ID).count(),
        1,
        "one entry state, one walk, one report",
    );

    // Two entry states are two keys and are each judged — and each answer is its own.
    let two = format!(
        "{HEAD}{cls}$a = new K(1);\n$b = new K(2);\n\
        \\PHPStan\\dumpType($a->value);\n\\PHPStan\\dumpType($b->value);\n"
    );
    assert_eq!(dumps(&two), vec!["dumped type: 1", "dumped type: 2"]);
    assert_eq!(findings(&two).into_iter().filter(|d| d.id == ID).count(), 2);
}

#[test]
fn the_memo_key_distinguishes_two_this_states() {
    // The `this:` component carries the seeded object's canonical rendering (C8), so a
    // delegate reached from two different `$this` states cannot replay one snapshot
    // for the other. `pre` differs between the two allocations, and the delegate's
    // own read of it is what would show a collision.
    let src = format!(
        "{HEAD}class Ms {{ public $pre = 0; public $out = 0;\n\
        \x20 public function __construct(int $p) {{ $this->pre = $p; $this->copy(); }}\n\
        \x20 private function copy(): void {{ $this->out = $this->pre; }} }}\n\
        $a = new Ms(1);\n$b = new Ms(2);\n\
        \\PHPStan\\dumpType($a->out);\n\\PHPStan\\dumpType($b->out);\n"
    );
    assert_eq!(dumps(&src), vec!["dumped type: 1", "dumped type: 2"]);
}

// ---------------------------------------------------------------------------
// The #385 guard, unmoved (D8).
// ---------------------------------------------------------------------------

#[test]
fn the_private_shape_guard_still_yields_unknown_through_a_delegate() {
    // The owner-probe shape, delegated: `view` is computed from an unknown argument,
    // so what crosses back is the walk's knowledge — which is nothing — and never the
    // declared `0`. The getter premises nothing against a declared `positive-int`.
    let cls = format!(
        "{HEAD}class Hd {{ private int $view = 0; private int $ad_count = 0;\n\
        \x20 public function __construct(int $original) {{ $this->fill($original); }}\n\
        \x20 private function fill(int $original): void {{ $this->view = $original - $this->ad_count; }}\n\
        \x20 public function getView(): int {{ return $this->view; }} }}\n\
        /** @param positive-int $n */\nfunction perPage(int $t, $n): int {{ return 1; }}\n"
    );
    assert_eq!(
        dumped(&format!(
            "{cls}function run(int $o): void {{ $h = new Hd($o);\n\\PHPStan\\dumpType($h->view); }}\n"
        )),
        "dumped type: unknown",
    );
    assert_eq!(
        count(&format!(
            "{cls}function run(int $o): void {{ $h = new Hd($o); $v = $h->getView(); perPage(10, $v); }}\n"
        )),
        0,
        "a computed slot convicts nobody, through a delegate as directly",
    );
}
