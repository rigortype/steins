//! Returned-allocation heap transfer (ADR-0057 T1, the outbound twin of ADR-0086):
//! a callee's allocation crosses back to the caller as a **fresh heap object** —
//! copy semantics, exactness copied never promoted, readonly bookkeeping
//! transferred, `escaped` false exactly when the return was the allocation's only
//! exit. The owner's probe shapes first, then §4's new-vs-factory equivalence, then
//! every §6 soundness leg and adversarial probe, then the v1 limits.
//!
//! Property facts are observed the way `object_state.rs` and `call_site_heap_entry.rs`
//! observe them — a dump, or a diagnostic at a typed sink, which fires only if the
//! heap kept the fact. The exactness-fed consumers (S2 `call.undefined-method`, the
//! arity family) need the absence family's boot surface, so they run through a
//! [`Boot`] mock the way `undefined_method.rs` and `arity.rs` do.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{
    CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNDEFINED_METHOD_ID, DEBUG_TYPE_ID, Diagnostic, Folder, ID,
    NoFold, READONLY_REASSIGNED_ID, check_project, check_with,
};
use steins_syntax::SourceTree;

fn findings(src: &str) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let input = SourceFile::new(&db, "main.php".to_owned(), src.to_owned());
    let project = Project::new(
        &db,
        vec![input],
        steins_db::ProjectLayout::fallback(),
        steins_db::PluginFacts::none(),
    );
    // `untyped.*` reports on the fixtures' own deliberately-untyped declarations,
    // not the behaviour under test — dropped to keep assertions stable.
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

/// Every dump a fixture asks for, in order.
fn dumps(src: &str) -> Vec<String> {
    findings(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message)
        .collect()
}

/// A boot-surface mock: the absence family's A9 gate open, no resident homonyms.
/// The exactness-gated ids (S2, arity) are silent under pure `NoFold` by design.
struct Boot;

impl Folder for Boot {
    fn fold(&mut self, _name: &str, _args: &[steins_syntax::ArgValue]) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn boot_surface_class_like(&mut self, _fqn: &str) -> Option<bool> {
        Some(false)
    }
}

/// The absence-family surface of a single-file fixture (`check_with`, boot ready).
fn absence(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "test.php", &mut Boot)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// The owner's flagship shape: a factory whose promoted prop is the bound argument.
const P: &str = "<?php\ndeclare(strict_types=1);\n\
    function needInt(int $x): void {}\n\
    function needString(string $s): void {}\n\
    class Foo { public function __construct(public mixed $n) {} }\n\
    function createFoo(int $n): Foo { return new Foo($n); }\n";

// ---------------------------------------------------------------------------
// The owner probe shapes (ADR-0057 §7 T1)
// ---------------------------------------------------------------------------

#[test]
fn the_factorys_property_is_the_bound_argument() {
    // `createFoo(123)->n` is `123` — the gap ADR-0057 §1 measured, at the assignment
    // rung where the rebind lands.
    assert_eq!(
        dumped(&format!("{P}$f = createFoo(123);\n\\PHPStan\\dumpType($f->n);\n")),
        "dumped type: 123",
    );

    // And it is a premise: the sink fires on the rebound prop.
    let sink = format!("{P}$f = createFoo(123);\nneedString($f->n);\n");
    let f = findings(&sink);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);

    // The value that fits stays silent — the finding follows the fact, not the shape.
    assert_eq!(count(&format!("{P}$f = createFoo(123);\nneedInt($f->n);\n")), 0);

    // The object itself types as its class.
    assert_eq!(dumped(&format!("{P}$f = createFoo(123);\n\\PHPStan\\dumpType($f);\n")), "dumped type: Foo");
}

#[test]
fn the_rebound_class_is_exact_enough_for_s2_and_arity() {
    // Rebound exactness feeds the definite-No consumers (ADR-0057 §6): a method that
    // is not there on the proven class is `call.undefined-method`…
    let s2 = absence(&format!("{P}$f = createFoo(123);\n$f->nope();\n"));
    assert_eq!(s2.len(), 1, "{s2:#?}");
    assert_eq!(s2[0].id, CALL_UNDEFINED_METHOD_ID);
    // …exactly as the inline `new` twin reports it.
    let inline = absence(&format!("{P}$a = new Foo(123);\n$a->nope();\n"));
    assert_eq!(inline.len(), 1, "{inline:#?}");
    assert_eq!(inline[0].id, CALL_UNDEFINED_METHOD_ID);
    assert_eq!(s2[0].message, inline[0].message);

    // …and a wrong-arity call on the rebound receiver reports too.
    let arity = absence(
        "<?php\nclass Foo { public function __construct(public $n) {} public function two($a, $b) {} }\n\
         function createFoo($n) { return new Foo($n); }\n$f = createFoo(1);\n$f->two(1);\n",
    );
    assert_eq!(arity.len(), 1, "{arity:#?}");
    assert_eq!(arity[0].id, CALL_TOO_FEW_ARGUMENTS_ID);
}

// ---------------------------------------------------------------------------
// §4 — `new` is the depth-0 summary: the equivalence pin
// ---------------------------------------------------------------------------

#[test]
fn a_factorys_object_is_the_inline_new_byte_for_byte() {
    // ADR-0057 §4 states the unification as a semantic identity: `new Foo(123)` is the
    // degenerate factory whose summary is assembled inline at depth 0. The two
    // fixtures are the same file with one rvalue swapped, so identical diagnostics —
    // ids, lines, columns and message text — is the whole claim, and it holds because
    // the factory's `return new Foo(...)` snapshot came out of the SAME
    // `new_heap_object` the assignment form runs.
    let inline = format!(
        "{P}$x = new Foo(123);\nneedString($x->n);\n\\PHPStan\\dumpType($x->n);\n\\PHPStan\\dumpType($x);\n"
    );
    let factory = format!(
        "{P}$x = createFoo(123);\nneedString($x->n);\n\\PHPStan\\dumpType($x->n);\n\\PHPStan\\dumpType($x);\n"
    );
    // Everything a reader sees, compared verbatim. The one field left out is the
    // dump's removal-fix payload, which is a byte range into the fixture's own text:
    // `createFoo(123)` is two characters longer than `new Foo(123)`, so the two files
    // simply are not the same length, and comparing offsets would test the fixture
    // rather than the transfer.
    let seen = |src: &str| -> Vec<(String, u32, u32, String)> {
        findings(src).into_iter().map(|d| (d.id.to_owned(), d.line, d.column, d.message)).collect()
    };
    assert_eq!(seen(&inline), seen(&factory), "the two roads must be indistinguishable");
    // Stated positively, so a future silence on BOTH sides cannot pass this test.
    assert_eq!(dumps(&factory), vec!["dumped type: 123", "dumped type: Foo"]);
}

// ---------------------------------------------------------------------------
// §6 — the soundness legs, one fixture each
// ---------------------------------------------------------------------------

#[test]
fn leg1_a_hooked_property_carries_nothing_back() {
    // FP class 16, inherited by construction: a `set` hook routes the write through
    // arbitrary code, so `new_heap_object` binds no fact and there is nothing for the
    // snapshot to carry. Pinned anyway — the crossing must not invent one.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Hk { public function __construct(public $n { set { $this->n = $value; } }) {} }\n\
        function makeHk($v): Hk { return new Hk($v); }\n$h = makeHk(1);\nneedString($h->n);\n";
    assert_eq!(count(src), 0, "a hooked prop binds no fact, so none crosses back");

    // The unhooked twin does cross — the silence above is the hook, not the seam.
    let plain = src.replace(" { set { $this->n = $value; } }", "");
    assert_eq!(count(&plain), 1);
}

#[test]
fn leg2_the_escape_bit_crosses_and_the_callers_sweep_honours_it() {
    // §2.1: `escaped` in the summary is escaped-BEFORE-return. A factory that let its
    // object out — here into a closure, which escapes without sweeping — rebinds
    // pre-escaped, so the caller's next unknown call sweeps it exactly as ADR-0036
    // requires of an object the caller leaked itself.
    const LEAKY: &str = "function leak(int $n): Foo { $f = new Foo($n); \
        $c = function() use ($f) { return $f; }; return $f; }\n";
    assert_eq!(
        dumped(&format!("{P}{LEAKY}$f = leak(5);\n\\PHPStan\\dumpType($f->n);\n")),
        "dumped type: 5",
        "escaped is not swept — it is swept by the NEXT unknown call",
    );
    assert_eq!(
        dumped(&format!("{P}{LEAKY}$f = leak(5);\nunknownFn();\n\\PHPStan\\dumpType($f->n);\n")),
        "dumped type: unknown",
    );

    // The unleaked factory is the contrast that makes the bit visible: the return was
    // the allocation's only exit, so the caller holds the sole reference and the same
    // unknown call cannot reach it (§1 — the precision payoff of ADR-0036, kept).
    assert_eq!(
        dumped(&format!("{P}$f = createFoo(5);\nunknownFn();\n\\PHPStan\\dumpType($f->n);\n")),
        "dumped type: 5",
    );

    // §2.2's other half: an object handed to an unknown call INSIDE the callee has
    // already had its props swept there, and the snapshot carries the post-sweep
    // state — nothing stale crosses.
    assert_eq!(
        dumped(&format!(
            "{P}function leak2(int $n): Foo {{ $f = new Foo($n); unknownFn($f); return $f; }}\n\
             $f = leak2(5);\n\\PHPStan\\dumpType($f->n);\n"
        )),
        "dumped type: unknown",
    );
}

#[test]
fn leg3_a_prop_fact_crosses_with_its_stratum() {
    // ADR-0052 amendment 1: the rebind is a derivation step whose only inputs are the
    // summary facts, so `min` over a singleton is the identity — an Asserted prop
    // rebinds Asserted and premises nothing on the proof layer.
    const CLAIM: &str = "/** @phpstan-assert 1 $x */\nfunction claimOne($x): void {}\n";
    let src = format!(
        "{P}{CLAIM}function mk(mixed $v): Foo {{ return new Foo($v); }}\n\
         function caller(mixed $x): void {{ claimOne($x); $f = mk($x); \\PHPStan\\dumpType($f->n); }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: 1 (asserted)");

    // And it premises no proof-layer finding, where the same value at `Verified` does.
    let quiet = format!(
        "{P}{CLAIM}function mk(mixed $v): Foo {{ return new Foo($v); }}\n\
         function caller(mixed $x): void {{ claimOne($x); $f = mk($x); needString($f->n); }}\n"
    );
    assert_eq!(count(&quiet), 0, "an Asserted prop is no proof premise");
    let loud = format!("{P}$f = createFoo(1);\nneedString($f->n);\n");
    assert_eq!(count(&loud), 1);
}

#[test]
fn leg4_exactness_is_copied_and_never_promoted() {
    // A1 verbatim. The lower-bound case: `$this` in a non-final class may be a
    // subclass instance, aliasing it does not promote it (ADR-0086 leg 3), and
    // returning it does not either — so S2, which gates on the bit, stays silent.
    let membership = "<?php\nclass Base { public $p;\n\
        \x20 public function m(): void { $that = $this; $x = idb($that); $x->nope(); } }\n\
        class Sub extends Base {}\nfunction idb(Base $b) { return $b; }\n";
    assert_eq!(absence(membership), vec![], "a lower bound must not be forged into exactness");

    // The same file with the class `final` — now `$this` IS exact, the copy carries
    // the bit, and the rebind reports. The difference is one keyword, which is what
    // locates the silence above in the exactness bit rather than in the crossing.
    let exact = "<?php\nfinal class Base { public $p;\n\
        \x20 public function m(): void { $that = $this; $x = idb($that); $x->nope(); } }\n\
        function idb(Base $b) { return $b; }\n";
    let f = absence(exact);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, CALL_UNDEFINED_METHOD_ID);

    // Chaining composes (§2.3): a callee that itself rebound an exact summary holds
    // `class_exact = true` and passes it up, through the composition arm…
    let chained = "<?php\nclass Foo { public function __construct(public $n) {} }\n\
        function inner($n) { return new Foo($n); }\n\
        function outer($n) { return inner($n); }\n$b = outer(1);\n$b->nope();\n";
    assert_eq!(absence(chained).len(), 1);
    // …and through a local, which is the same claim with the summary parked in the
    // callee's own store first.
    let via_local = "<?php\nclass Foo { public function __construct(public $n) {} }\n\
        function inner($n) { return new Foo($n); }\n\
        function outer($n) { $x = inner($n); return $x; }\n$b = outer(1);\n$b->nope();\n";
    assert_eq!(absence(via_local).len(), 1);
}

#[test]
fn leg5_readonly_bookkeeping_transfers() {
    // The language guarantee that justified sweep immunity in ADR-0036 does not stop
    // at a `return`: through an ESCAPED factory (so the sweep actually runs) the
    // readonly prop survives what the mutable one does not.
    const RO: &str = "<?php\ndeclare(strict_types=1);\n\
        class R { public function __construct(public readonly int $ro, public mixed $m) {} }\n\
        function makeR(int $n): R { $r = new R($n, $n); \
        $c = function() use ($r) { return $r; }; return $r; }\n";
    assert_eq!(
        dumps(&format!("{RO}$r = makeR(7);\nunknownFn();\n\\PHPStan\\dumpType($r->ro);\n\\PHPStan\\dumpType($r->m);\n")),
        vec!["dumped type: 7", "dumped type: unknown"],
    );

    // And the write bookkeeping crosses with it: the constructor established `ro`, so
    // a caller-side assignment is a proven second write.
    let reassign = "<?php\nclass R { public function __construct(public readonly int $ro) {} }\n\
        function makeR(int $n): R { return new R($n); }\n$r = makeR(7);\n$r->ro = 9;\n";
    let f = findings(reassign);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, READONLY_REASSIGNED_ID);
}

#[test]
fn leg6_the_join_keeps_only_what_every_path_proves() {
    // §2.4's adversarial probe: conditionally returns param-vs-fresh. Same class on
    // both arms, so the object survives; the prop survives only where the two agree.
    const PICK: &str = "<?php\nclass Foo { public function __construct(public mixed $n) {} }\n";
    let agree = format!(
        "{PICK}function pick(Foo $x, bool $b): Foo {{ if ($b) {{ return $x; }} return new Foo(1); }}\n\
         function caller(bool $b): void {{ $a = new Foo(1); $r = pick($a, $b); \\PHPStan\\dumpType($r->n); }}\n"
    );
    assert_eq!(dumped(&agree), "dumped type: 1");
    let differ = format!(
        "{PICK}function pick(Foo $x, bool $b): Foo {{ if ($b) {{ return $x; }} return new Foo(2); }}\n\
         function caller(bool $b): void {{ $a = new Foo(1); $r = pick($a, $b); \\PHPStan\\dumpType($r->n); }}\n"
    );
    assert_eq!(dumped(&differ), "dumped type: 1|2", "the value-domain join, per prop");

    // Exactness dies on the param arm unless the argument's own object was exact —
    // which under ADR-0086 it is, the copy carrying the caller's `new`'s bit. The bit
    // is COPIED, so the pair below is the honest reading of it: nothing in the join
    // promotes, and where an arm is a lower bound the join is one too (leg 4).
    let exact_both = format!(
        "{PICK}function pick(Foo $x, bool $b): Foo {{ if ($b) {{ return $x; }} return new Foo(1); }}\n\
         function caller(bool $b): void {{ $a = new Foo(1); $r = pick($a, $b); $r->nope(); }}\n"
    );
    assert_eq!(absence(&exact_both).len(), 1, "both arms exact `Foo`");

    // Differing classes ⇒ no heap summary at all (§2.4): a joined "one of A or B" is
    // the Member-fact shape, not the heap's, and the arm floor already carries it.
    let classes = "<?php\nclass A { public $n = 1; }\nclass B { public $n = 2; }\n\
        function pick(bool $b) { if ($b) { return new A(); } return new B(); }\n\
        function caller(bool $b): void { $r = pick($b); \\PHPStan\\dumpType($r->n); }\n";
    assert_eq!(dumped(classes), "dumped type: unknown");
}

#[test]
fn leg6_any_non_allocation_path_kills_the_summary() {
    // §2.5: a `null`-returning path means the call result is not always this object,
    // and there is no heap shape that truthfully covers both — no summary, arm floor.
    // The condition must be UNDECIDED, or the walk prunes the null arm as dead and
    // the remaining exit is honestly the only one there is (the next assertion).
    let nullable = format!(
        "{P}function maybeFoo(int $n, bool $b): ?Foo {{ if ($b) {{ return new Foo($n); }} return null; }}\n\
         function caller(bool $b): void {{ $f = maybeFoo(1, $b); \\PHPStan\\dumpType($f->n); }}\n"
    );
    assert_eq!(dumped(&nullable), "dumped type: unknown");

    // The same body under a DECIDED guard keeps the summary, and rightly: the null
    // path is proven unreachable for this binding, so no exit is being hidden.
    let decided = format!(
        "{P}function maybeFoo(int $n): ?Foo {{ if ($n > 0) {{ return new Foo($n); }} return null; }}\n\
         $f = maybeFoo(1);\n\\PHPStan\\dumpType($f->n);\n"
    );
    assert_eq!(dumped(&decided), "dumped type: 1");

    // A scalar path kills it the same way; so does an untyped fall-through.
    let mixed_exit = "<?php\nclass Foo { public function __construct(public mixed $n) {} }\n\
        function f(bool $b) { if ($b) { return new Foo(1); } return 0; }\n\
        function caller(bool $b): void { $r = f($b); \\PHPStan\\dumpType($r->n); }\n";
    assert_eq!(dumped(mixed_exit), "dumped type: unknown");
    let fallthrough = "<?php\nclass Foo { public function __construct(public mixed $n) {} }\n\
        function f(bool $b) { if ($b) { return new Foo(1); } }\n\
        function caller(bool $b): void { $r = f($b); \\PHPStan\\dumpType($r->n); }\n";
    assert_eq!(dumped(fallthrough), "dumped type: unknown");
}

#[test]
fn leg7_the_summary_replays_and_recursion_degrades() {
    // The replayability leg (§6.7 / ADR-0048 §2): the summary depends on the caller
    // only through the bound entry state, so two call sites under one key answer
    // identically — the second is a memo hit, and a memo hit REPLAYS.
    assert_eq!(
        dumps(&format!(
            "{P}$a = createFoo(1);\n$b = createFoo(1);\n\\PHPStan\\dumpType($a->n);\n\\PHPStan\\dumpType($b->n);\n"
        )),
        vec!["dumped type: 1", "dumped type: 1"],
    );
    // Different entry states answer differently — the key names the state, and no
    // allocation id is in it.
    assert_eq!(
        dumps(&format!(
            "{P}$a = createFoo(1);\n$b = createFoo(2);\n\\PHPStan\\dumpType($a->n);\n\\PHPStan\\dumpType($b->n);\n"
        )),
        vec!["dumped type: 1", "dumped type: 2"],
    );

    // Recursion: the inner key is on the descent stack, so the inner call yields no
    // summary; that exit is then not an allocation and the outer summary dies with it
    // (§3's stricter heap rule, not A5's value degradation). Arm floor; terminates.
    let rec = format!(
        "{P}function rf(int $n, bool $b): Foo {{ if ($b) {{ return new Foo($n); }} return rf($n, $b); }}\n\
         function caller(bool $b): void {{ $f = rf(1, $b); \\PHPStan\\dumpType($f->n); }}\n"
    );
    assert_eq!(dumped(&rec), "dumped type: unknown");
}

// ---------------------------------------------------------------------------
// The remaining §6 adversarial probes
// ---------------------------------------------------------------------------

#[test]
fn a_factory_mutating_global_state_leaks_nothing_into_its_summary() {
    // §6's probe: the summary describes only the returned allocation. Caller-side
    // static-property channels are invalidated by every call anyway (ADR-0052 §7) and
    // superglobal effects are the effect system's business (ADR-0055) — so a factory
    // that runs an unknown mutator before allocating still hands back a clean object.
    let src = format!(
        "{P}function mut(int $n): Foo {{ unknownGlobalWriter(); return new Foo($n); }}\n\
         $f = mut(4);\n\\PHPStan\\dumpType($f->n);\n"
    );
    assert_eq!(dumped(&src), "dumped type: 4");
}

#[test]
fn a_factory_storing_into_a_static_ends_earlier_than_the_escape_rule() {
    // §6's probe expects "escaped-before-return ⇒ rebound pre-escaped". The store
    // never gets that far: a static-property write is a barrier for the walk, so the
    // callee's binding for the object is gone by the `return` and there is no
    // allocation to snapshot at all. No summary, arm floor — the safe side of the
    // same question, and pinned here so the silence is not read as a lost escape bit.
    let src = format!(
        "{P}class Reg {{ public static mixed $held = null; }}\n\
         function stash(int $n): Foo {{ $f = new Foo($n); Reg::$held = $f; return $f; }}\n\
         $f = stash(5);\n\\PHPStan\\dumpType($f->n);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
    // The escape route that IS observable is leg 2's closure capture; the two probes
    // together cover the clause from both sides.
}

#[test]
fn a_fluent_setter_chain_gets_class_continuity_and_no_forged_exactness() {
    // `return $this` (§6's probe): pre-escaped by construction, and exact only where
    // the receiver leg proved the receiver exact — which an allocation-proven
    // `Receiver::Var` does (ADR-0086 §3).
    const B: &str = "<?php\nclass B { public mixed $v = null;\n\
        \x20 public function set(int $n): B { $this->v = $n; return $this; } }\n";
    assert_eq!(dumped(&format!("{B}$b = new B();\n$c = $b->set(3);\n\\PHPStan\\dumpType($c);\n")), "dumped type: B");
    // Class continuity is the claim; the exact class is the receiver's own, so S2 has
    // its premise on the chained result.
    let f = absence(&format!("{B}$b = new B();\n$c = $b->set(3);\n$c->nope();\n"));
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, CALL_UNDEFINED_METHOD_ID);

    // A `$this`-origin receiver proves no object (ADR-0086 §3), so the method it calls
    // gets the class shell and nothing forged: the chained result is a lower bound and
    // S2 has no premise.
    let inner = "<?php\nclass B { public mixed $v = null;\n\
        \x20 public function set(int $n): B { $this->v = $n; return $this; }\n\
        \x20 public function go(): void { $c = $this->set(3); $c->nope(); } }\n\
        class C extends B {}\n";
    assert_eq!(absence(inner), vec![], "a `$this`-origin chain forges no exactness");
}

// ---------------------------------------------------------------------------
// The inbound/outbound seam (ADR-0086) and the method rung (ADR-0075)
// ---------------------------------------------------------------------------

#[test]
fn an_identity_function_hands_back_a_copy_of_what_crossed_in() {
    // ADR-0057 §2.3: origin does not matter. `id`'s `$b` is the copy ADR-0086 §2
    // seeded, and the summary is a snapshot of what the walk holds about it — so
    // `$y` carries `value = 1` with the exactness the copy carried.
    let src = "<?php\ndeclare(strict_types=1);\n\
        class Box { public function __construct(public mixed $value) {} }\n\
        function id(Box $b): Box { return $b; }\n$x = new Box(1);\n$y = id($x);\n\
        \\PHPStan\\dumpType($y->value);\n";
    assert_eq!(dumped(src), "dumped type: 1");

    // The caller's own `$x` is swept by the call exactly as before — ADR-0086 leg 4
    // is unchanged, and the copy flowing back does not undo it.
    let both = "<?php\ndeclare(strict_types=1);\n\
        class Box { public function __construct(public mixed $value) {} }\n\
        function id(Box $b): Box { return $b; }\n$x = new Box(1);\n$y = id($x);\n\
        \\PHPStan\\dumpType($x->value);\n";
    assert_eq!(dumped(both), "dumped type: unknown");
}

#[test]
fn a_method_factory_rebinds_where_a_functions_does() {
    // ADR-0075's whole point: one seam. The method road inherits the rebind rather
    // than repeating it.
    let src = format!(
        "{P}final class Factory {{ public function make(int $n): Foo {{ return new Foo($n); }} }}\n\
         $fac = new Factory();\n$f = $fac->make(123);\n\\PHPStan\\dumpType($f->n);\n"
    );
    assert_eq!(dumped(&src), "dumped type: 123");

    // A static factory takes the same rung.
    let stat = format!(
        "{P}final class Factory {{ public static function make(int $n): Foo {{ return new Foo($n); }} }}\n\
         $f = Factory::make(123);\n\\PHPStan\\dumpType($f->n);\n"
    );
    assert_eq!(dumped(&stat), "dumped type: 123");
}

// ---------------------------------------------------------------------------
// v1 limits, each pinned with its reason
// ---------------------------------------------------------------------------

#[test]
fn the_direct_forms_are_unchanged_in_v1() {
    // ADR-0057 T1 amendment B5: value and argument position have no store to rebind
    // into, so the crossing is not observable there. `dumpType(createFoo(123))` reads
    // the declared return as it always has — the same exclusion ADR-0075 §3 took for
    // method calls in value position, and one layer below this ADR.
    assert_eq!(dumped(&format!("{P}\\PHPStan\\dumpType(createFoo(123));\n")), "dumped type: Foo");
    // A property read off the direct form is not a value either.
    assert_eq!(
        dumped(&format!("{P}function g(): int {{ return 0; }}\n\\PHPStan\\dumpType(createFoo(123)->n);\n")),
        "dumped type: unknown",
    );
    // Argument position stays silent for the same reason: the sink that fires on the
    // assignment form does not fire here.
    assert_eq!(count(&format!("{P}needString(createFoo(123)->n);\n")), 0);
    let assigned = format!("{P}$f = createFoo(123);\nneedString($f->n);\n");
    assert_eq!(count(&assigned), 1, "the rung that DOES answer, for the contrast");
}

#[test]
fn a_generator_hands_back_no_allocation() {
    // §5: the return value IS the Generator; yielded objects cross a different
    // boundary. Both summary components refuse.
    let src = format!(
        "{P}function gen(int $n) {{ yield 1; return new Foo($n); }}\n\
         $g = gen(1);\n\\PHPStan\\dumpType($g->n);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
}

#[test]
fn the_constructors_own_writes_still_beat_a_literal_default() {
    // The #377 rule rides along, because the snapshot for `return new Foo(...)` is
    // minted through `new_heap_object` rather than assembled anew: a default the
    // constructor overwrites is dropped there and so never reaches the caller. A
    // stale `Verified` 0 on a factory's object would be a proof-layer false positive
    // one boundary further from where it was written.
    let src = "<?php\nclass D { public $view = 0; public $kept = 3;\n\
        \x20 public function __construct(int $v) { $this->view = $v; } }\n\
        function mkD(int $v): D { return new D($v); }\n\
        $d = mkD(5);\n\\PHPStan\\dumpType($d->view);\n\\PHPStan\\dumpType($d->kept);\n";
    assert_eq!(dumps(src), vec!["dumped type: unknown", "dumped type: 3"]);
}
