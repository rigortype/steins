//! Call-site heap entry (ADR-0086): an object crosses the binding descent as a
//! **copy**, under a fresh callee-local allocation. The **argument leg** (§2) binds
//! that copy to the parameter's name; the **receiver leg** (§3) binds it to `$this`,
//! because the receiver is the zeroth argument. The soundness legs of ADR-0086 §5,
//! one test each, numbered, then the receiver leg's own section at the end.
//!
//! Property facts are observed the way `object_state.rs` observes them — through a
//! diagnostic at a typed sink (`needInt($o->p)` / `needString($o->p)`), which fires
//! only if the heap kept the fact. Every fixture declares `strict_types=1`: in
//! coercive mode an `int` reaches a `string` parameter by conversion and no engine
//! reports it, so the coercive surface cannot witness anything here.

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
    let ds: Vec<Diagnostic> =
        findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one dump, got {ds:?}");
    ds[0].message.clone()
}

/// Two typed sinks and a plain box. `Box`'s value is promoted and untyped, so what
/// the heap holds for it is exactly what the `new` site proved.
const PRELUDE: &str = "<?php\ndeclare(strict_types=1);\n\
    function needInt(int $x): void {}\n\
    function needString(string $s): void {}\n\
    class Box { public function __construct(public mixed $value) {} }\n";

// Leg 1 — aliasing among the arguments survives the crossing.

#[test]
fn leg1_the_same_object_passed_twice_shares_one_callee_copy() {
    // Two independent copies would convict correct code: `$y->value` IS `'s'` after
    // the write through `$x`, because `$x` and `$y` are one object. One copy per
    // distinct caller allocation (ADR-0086 §2) is what keeps this silent.
    let src = format!(
        "{PRELUDE}function f(Box $x, Box $y): void {{ $x->value = 's'; needString($y->value); }}\n\
         $b = new Box(1);\nf($b, $b);\n"
    );
    assert_eq!(count(&src), 0, "two copies for one caller object convict correct code");

    // And the aliasing is real rather than incidental: the write is visible through
    // the second name, so a *wrong* read through it does fire.
    let wrong = src.replace("needString($y->value)", "needInt($y->value)");
    let f = findings(&wrong);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);

    // Two DIFFERENT caller objects get two copies — the write through one must not
    // be visible through the other.
    let distinct = format!(
        "{PRELUDE}function f(Box $x, Box $y): void {{ $x->value = 's'; needInt($y->value); }}\n\
         $b = new Box(1);\n$c = new Box(1);\nf($b, $c);\n"
    );
    assert_eq!(count(&distinct), 0, "$y kept its own 1, which needInt accepts");
}

#[test]
fn leg1_a_method_call_shares_its_argument_copies_too() {
    // `descend` is one seam, so the method road inherits the rule rather than
    // repeating it.
    let src = format!(
        "{PRELUDE}final class H {{\n\
         \x20 public function f(Box $x, Box $y): void {{ $x->value = 's'; needString($y->value); }}\n\
         }}\n$h = new H();\n$b = new Box(1);\n$h->f($b, $b);\n"
    );
    assert_eq!(count(&src), 0);
}

// Leg 2 — an escaped object keeps its non-readonly props to itself.

#[test]
fn leg2_an_escaped_object_crosses_only_its_readonly_props() {
    // Stored into another object's property, so an alias the callee cannot see
    // exists; then written, so the caller's non-readonly prop is fresher than any
    // name the descent could track. `value` must not cross. `ro` is immune by the
    // language guarantee and does cross — which is what makes the silence above
    // attributable to the escape rather than to the seed failing wholesale.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Slot { public mixed $held = null; }\n\
        class Pair { public mixed $value = null; public function __construct(public readonly int $ro) {} }\n\
        function f(Pair $p): void { needString($p->value); needString($p->ro); }\n\
        $s = new Slot();\n$p = new Pair(7);\n$s->held = $p;\n$p->value = 1;\nf($p);\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "only the readonly prop crossed: {f:#?}");
    assert_eq!(f[0].id, ID);
    assert!(f[0].message.contains('7'), "the finding is the readonly 7, not the swept 1: {f:#?}");

    // Drop the escape and the non-readonly prop crosses too — two findings.
    let local = src.replace("$s->held = $p;\n", "");
    assert_eq!(count(&local), 2, "an unescaped object crosses both props");
}

// Leg 3 — exactness is copied, never promoted.

#[test]
fn leg3_a_laundered_this_alias_seeds_a_non_exact_copy() {
    // `$this` in a non-final class is a lower bound (audit G1): the running instance
    // may be a subclass. Aliasing it into a variable must not promote it, and the
    // No-side consumer that notices is `instanceof`: over an EXACT `Base` the engine
    // proves `instanceof Sub` false and the branch never contributes an exit; over a
    // lower bound it must answer Maybe and both arms return. The summary is the
    // read-out, so nothing but the copied exactness bit can move it.
    const F: &str =
        "function f(Base $x): mixed { if ($x instanceof Sub) { return 1; } return 's'; }\n";
    let classes = format!(
        "<?php\ndeclare(strict_types=1);\n\
         class Base {{ public mixed $p; }}\nclass Sub extends Base {{}}\n{F}"
    );
    assert_eq!(
        dumped(&format!("{classes}\\PHPStan\\dumpType(f(new Base()));")),
        "dumped type: 's'",
        "an exact copy decides the instanceof",
    );

    let laundered = format!(
        "<?php\ndeclare(strict_types=1);\n\
         class Base {{\n\
         \x20 public mixed $p;\n\
         \x20 public function m(): void {{ $that = $this; \\PHPStan\\dumpType(f($that)); }}\n\
         }}\nclass Sub extends Base {{}}\n{F}"
    );
    assert_eq!(
        dumped(&laundered),
        "dumped type: 1|'s'",
        "a `$this` alias is a lower bound, and the copy must not promote it",
    );
}

// Leg 4 — the caller-side sweep is untouched (ADR-0086 §2's stated refusal).

#[test]
fn leg4_the_call_still_sweeps_the_callers_object() {
    // The descent walked `mutate` and could see it writes only `$b->value`; the ADR
    // refuses to spend that on the caller's fact (a per-parameter non-mutation proof
    // is ADR-0055 Part II's job). So after the call the caller knows nothing, and a
    // read that would have fired on the pre-call fact stays silent.
    let src = format!(
        "{PRELUDE}function mutate(Box $b): void {{ $b->value = 's'; }}\n\
         $b = new Box(1);\nmutate($b);\nneedString($b->value);\n"
    );
    assert_eq!(count(&src), 0, "the caller's prop was swept by the call, as before");

    // And the dump surface says so in as many words.
    let dump = format!(
        "{PRELUDE}function mutate(Box $b): void {{ $b->value = 's'; }}\n\
         $b = new Box(1);\nmutate($b);\n\\PHPStan\\dumpType($b->value);\n"
    );
    assert_eq!(dumped(&dump), "dumped type: unknown");
}

// Leg 5 — the key names the entry state, and each state emits once.

#[test]
fn leg5_the_binding_key_distinguishes_two_entry_states() {
    // `h(new Box(1))` fires inside `h`; `h(new Box('s'))` does not. A key naming only
    // the class would replay the first walk's summary for the second and suppress the
    // second's emission (ADR-0075 §2.1) — or worse, fire on it.
    let src = format!(
        "{PRELUDE}function h(Box $b): void {{ needString($b->value); }}\n\
         h(new Box(1));\nh(new Box('s'));\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 1, "exactly the int entry state fires: {f:#?}");
    assert_eq!(f[0].id, ID);

    // Reversed, to show the answer follows the argument and not the call order.
    let rev = format!(
        "{PRELUDE}function h(Box $b): void {{ needString($b->value); }}\n\
         h(new Box('s'));\nh(new Box(1));\n"
    );
    assert_eq!(count(&rev), 1);

    // The same entry state twice on one line is one finding: same key, same site,
    // same provenance.
    let twice = format!(
        "{PRELUDE}function h(Box $b): void {{ needString($b->value); }}\n\
         h(new Box(1)); h(new Box(1));\n"
    );
    assert_eq!(count(&twice), 1, "one key, one emission");
}

// Leg 6 — strata cross with their facts.

#[test]
fn leg6_an_asserted_prop_stays_asserted_inside_the_callee() {
    // A prop written from an asserted value is Asserted (ADR-0052 §5), and the copy
    // carries that stratum in rather than laundering it to Verified. The summary is
    // where it shows: an Asserted premise yields an Asserted summary.
    const CLAIM: &str = "/** @phpstan-assert 1 $x */\nfunction claimOne($x): void {}\n";
    let src = format!(
        "{PRELUDE}{CLAIM}function g(Box $b): mixed {{ return $b->value; }}\n\
         function caller(mixed $x): void {{ claimOne($x); $b = new Box($x); \\PHPStan\\dumpType(g($b)); }}\n"
    );
    assert_eq!(dumped(&src), "dumped type: 1 (asserted)");

    // Written after construction rather than through it — same clause, same answer.
    let written = format!(
        "{PRELUDE}{CLAIM}function g(Box $b): mixed {{ return $b->value; }}\n\
         function caller(mixed $x): void {{ claimOne($x); $b = new Box(0); $b->value = $x;\n\
         \\PHPStan\\dumpType(g($b)); }}\n"
    );
    assert_eq!(dumped(&written), "dumped type: 1 (asserted)");

    // The literal twin, for the contrast that makes the stratum visible.
    let verified = format!(
        "{PRELUDE}function g(Box $b): mixed {{ return $b->value; }}\n\
         \\PHPStan\\dumpType(g(new Box(1)));\n"
    );
    assert_eq!(dumped(&verified), "dumped type: 1");

    // And an Asserted prop premises no proof-layer finding inside the callee, where
    // the very same entry state at `Verified` does.
    let quiet = format!(
        "{PRELUDE}{CLAIM}function h(Box $b): void {{ needString($b->value); }}\n\
         function caller(mixed $x): void {{ claimOne($x); $b = new Box($x); h($b); }}\n"
    );
    assert_eq!(count(&quiet), 0, "an Asserted prop is no proof premise");
    let loud =
        format!("{PRELUDE}function h(Box $b): void {{ needString($b->value); }}\nh(new Box(1));\n");
    assert_eq!(count(&loud), 1);
}

// Leg 7 — hooked properties never cross, because they never enter the heap.

#[test]
fn leg7_a_hooked_property_carries_nothing_in() {
    // FP class 16, inherited by construction: a `set` hook routes the write through
    // arbitrary code, so `build_new_object` binds no fact and there is nothing for
    // the copy to carry. Pinned anyway — the seed must not invent one.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Hk { public function __construct(public $n { set { $this->n = $value; } }) {} }\n\
        function f(Hk $h): void { needString($h->n); }\n\
        f(new Hk(1));\n";
    assert_eq!(count(src), 0, "a hooked prop binds no fact, so none crosses");

    // The unhooked twin does cross — the silence above is the hook, not the seam.
    let plain = src.replace(" { set { $this->n = $value; } }", "");
    assert_eq!(count(&plain), 1);
}

// Leg 8 — the three probes ADR-0086 §1 measured as silent.

#[test]
fn leg8_probe_one_an_argument_object_carries_its_props_in() {
    // ADR-0086 §1 consequence 1, and §1 consequence 3 with it: the call has no
    // bindable value argument at all, so before this slice it did not descend.
    let src = format!(
        "{PRELUDE}function h(Box $b): void {{ needString($b->value); }}\nh(new Box(1));\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);

    // The T0 convention of adding an unused `int $trigger` to make an object-only
    // call descend is retired with this slice: the same fixture answers the same way
    // with and without one.
    let triggered = format!(
        "{PRELUDE}function h(Box $b, int $trigger): void {{ needString($b->value); }}\n\
         h(new Box(1), 0);\n"
    );
    assert_eq!(count(&triggered), 1, "the trigger param changes nothing now");

    // A heap-bound variable carries in exactly as a direct `new` does.
    let via_var =
        format!("{PRELUDE}function h(Box $b): void {{ needString($b->value); }}\n$b = new Box(1);\nh($b);\n");
    assert_eq!(count(&via_var), 1);
}

#[test]
fn leg8_probe_two_a_property_read_becomes_a_return_fact() {
    // ADR-0086 §1 consequence 2. `return_value_fact`'s `PropFetch` arm and
    // `join_summary` needed no change — the store they read is no longer empty.
    let src =
        format!("{PRELUDE}function g(Box $b): mixed {{ return $b->value; }}\n\\PHPStan\\dumpType(g(new Box(1)));\n");
    assert_eq!(dumped(&src), "dumped type: 1");

    // And the summary rebinds through an assignment on the same rung.
    let assigned = format!(
        "{PRELUDE}function g(Box $b): mixed {{ return $b->value; }}\n\
         $v = g(new Box('ok'));\n\\PHPStan\\dumpType($v);\n"
    );
    assert_eq!(dumped(&assigned), "dumped type: 'ok'");
}

// (The third probe — the `template-type` conformance shape — is pinned where its
// family lives: `generics_carry::the_conformance_shape_reads_and_now_enforces`.)

// Leg 9 — the budget bounds the new walk.

#[test]
fn leg9_a_recursive_object_passing_pair_degrades_to_the_floor() {
    // An object-only call descends now, so mutual recursion through one is reachable
    // where it was not. The on-stack binding-key guard and `MAX_BINDING_DEPTH` bound
    // it exactly as they bound a value binding's: no summary, arm floor, no loop.
    let src = format!(
        "{PRELUDE}function a(Box $b): mixed {{ return c($b); }}\n\
         function c(Box $b): mixed {{ return a($b); }}\n\
         \\PHPStan\\dumpType(a(new Box(1)));\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");

    // Direct self-recursion likewise.
    let selfrec = format!(
        "{PRELUDE}function r(Box $b): mixed {{ return r($b); }}\n\\PHPStan\\dumpType(r(new Box(1)));\n"
    );
    assert_eq!(dumped(&selfrec), "dumped type: unknown");
}

// The callee's own writes are its own to read.

#[test]
fn a_callee_reads_the_write_it_just_made() {
    // The descent has always recorded prop writes into its store (only the *checks*
    // are descent-gated), so this is right by construction — pinned because the copy
    // is what gives the write somewhere to land.
    let src = format!(
        "{PRELUDE}function w(Box $b): void {{ $b->value = 1; needString($b->value); }}\n\
         w(new Box('s'));\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 1, "the callee sees its own write, not what came in: {f:#?}");
    assert_eq!(f[0].id, ID);
}

// The copy is a copy: no callee-side write reaches the caller.

#[test]
fn a_callee_side_write_is_invisible_to_the_caller() {
    // ADR-0086 §2's copy argument, in the direction the caller can observe: `$b`'s
    // allocation is the caller's, `f`'s is a fresh callee-local one, and the two
    // never meet. (The caller's fact is swept by the call anyway — leg 4 — so this
    // pins that the sweep is not being *undone* by a write flowing back.)
    let src = format!(
        "{PRELUDE}function f(Box $b): void {{ $b->value = 's'; }}\n\
         $b = new Box(1);\nf($b);\n\\PHPStan\\dumpType($b->value);\n"
    );
    assert_eq!(dumped(&src), "dumped type: unknown");
}

// ---------------------------------------------------------------------------
// The receiver leg (ADR-0086 §3): `$this` is the zeroth argument's copy.
// ---------------------------------------------------------------------------

/// [`PRELUDE`]'s box, plus the methods the receiver leg is about. `unwrap()` and
/// `get()` read `$this->value` into a return; `m()` sinks it; `reset()` throws it
/// away. `Box` is deliberately **not** final: an allocation-proven receiver
/// dispatches exactly without it, and the seed rides on that same proof.
const BOXM: &str = "<?php\ndeclare(strict_types=1);\n\
    function needInt(int $x): void {}\n\
    function needString(string $s): void {}\n\
    class Box {\n\
    \x20 public function __construct(public mixed $value) {}\n\
    \x20 public function unwrap(): mixed { return $this->value; }\n\
    \x20 public function get(): mixed { return $this->value; }\n\
    \x20 public function m(): void { needString($this->value); }\n\
    \x20 public function reset(): void { $this->value = null; }\n\
    }\n";

#[test]
fn the_receiver_reads_exactly_as_the_argument_does() {
    // The parity ADR-0086 §3 exists for: `$box->unwrap()` and `unwrap($box)` are the
    // same call with the receiver written in a different place, and diverging on which
    // side of the arrow the object sits is a difference no user can account for.
    // Read at the assignment rung, which is where a method's summary is observable at
    // all (ADR-0075 §3 — see the exclusion pinned below).
    let receiver = format!("{BOXM}$b = new Box(1);\n$v = $b->unwrap();\nneedString($v);\n");
    let argument = format!(
        "{BOXM}function unwrap(Box $b): mixed {{ return $b->value; }}\n\
         $b = new Box(1);\n$v = unwrap($b);\nneedString($v);\n"
    );
    let (r, a) = (findings(&receiver), findings(&argument));
    assert_eq!(r.len(), 1, "{r:#?}");
    assert_eq!(a.len(), 1, "{a:#?}");
    assert_eq!(r[0].id, ID);
    assert_eq!(r[0].id, a[0].id, "the two forms must report the same finding");
    // The receiver whose value fits stays silent on both roads — the finding follows
    // the value, not the shape.
    assert_eq!(count(&format!("{BOXM}$b = new Box('s');\n$v = $b->unwrap();\nneedString($v);\n")), 0);

    // `return $this->value` is a summary fact now, so the ZERO-argument call — which
    // could not descend at all before, having nothing bound — answers with the value.
    assert_eq!(
        dumped(&format!("{BOXM}$b = new Box(1);\n$v = $b->get();\n\\PHPStan\\dumpType($v);")),
        "dumped type: 1",
    );
    // Its argument twin, for the same reason and the same answer.
    assert_eq!(
        dumped(&format!(
            "{BOXM}function get(Box $b): mixed {{ return $b->value; }}\n\
             $b = new Box(1);\n\\PHPStan\\dumpType(get($b));"
        )),
        "dumped type: 1",
    );

    // And a sink *inside* the method fires on the receiver's own proven prop — the
    // one form that needs no summary to cross a value surface at all.
    let inside = format!("{BOXM}$b = new Box(1);\n$b->m();\n");
    let f = findings(&inside);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
    assert_eq!(count(&format!("{BOXM}$b = new Box('s');\n$b->m();\n")), 0);
}

#[test]
fn a_method_call_in_value_position_is_still_the_value_ir_limit() {
    // The parity above is read at the assignment rung because the direct form is
    // blocked one layer below this slice: `ArgValue::Call` carries a **simple function
    // name** and a method call never reaches the value IR at all (ADR-0075 §3, v1
    // exclusion; the same carrier limit issue #374 measured for `Receiver::New`). The
    // summary the receiver leg now proves is therefore invisible in argument and dump
    // position, and pinning that here says which layer to fix rather than leaving the
    // gap to be re-diagnosed as a heap-entry one.
    assert_eq!(
        dumped(&format!("{BOXM}$b = new Box(1);\n\\PHPStan\\dumpType($b->get());")),
        "dumped type: unknown",
    );
    assert_eq!(
        count(&format!("{BOXM}$b = new Box(1);\nneedString($b->unwrap());\n")),
        0,
        "a method call in argument position is not a value here",
    );
    // The function twin of the very same body does answer, which is what locates the
    // limit in the carrier rather than in the descent.
    assert_eq!(
        dumped(&format!(
            "{BOXM}function get(Box $b): mixed {{ return $b->value; }}\n\
             $b = new Box(1);\n\\PHPStan\\dumpType(get($b));"
        )),
        "dumped type: 1",
    );
}

#[test]
fn the_receiver_shares_its_copy_with_an_argument_naming_it() {
    // ADR-0086 §3's aliasing clause: `$b->m($b)` names one object twice, so `$this`
    // and the parameter must be ONE callee allocation. Two copies would convict
    // correct code — the write through `$this` IS visible through `$o`.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Box {\n\
        \x20 public function __construct(public mixed $value) {}\n\
        \x20 public function m(Box $o): void { $this->value = 's'; needString($o->value); }\n\
        }\n$b = new Box(1);\n$b->m($b);\n";
    assert_eq!(count(src), 0, "$this and the parameter are one object");

    // The mirror — write through the parameter, read through `$this` — is the same
    // claim from the other side, and the same one allocation answers it.
    let mirror = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Box {\n\
        \x20 public function __construct(public mixed $value) {}\n\
        \x20 public function m(Box $o): void { $o->value = 's'; needString($this->value); }\n\
        }\n$b = new Box(1);\n$b->m($b);\n";
    assert_eq!(count(mirror), 0, "the alias runs both ways");

    // And the aliasing is real rather than incidental: with two DIFFERENT objects the
    // write through one is not visible through the other, so the read fires.
    let distinct = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Box {\n\
        \x20 public function __construct(public mixed $value) {}\n\
        \x20 public function m(Box $o): void { $this->value = 's'; needString($o->value); }\n\
        }\n$b = new Box(1);\n$c = new Box(1);\n$b->m($c);\n";
    let f = findings(distinct);
    assert_eq!(f.len(), 1, "$c kept its own 1: {f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn a_this_origin_receiver_seeds_exactly_as_before() {
    // `$this->inner()` is `Receiver::This`, which proves no object and seeds through
    // `seed_this_object` as it always has. The consequence is visible and deliberate:
    // a prop written in `outer` is NOT visible inside `inner`. `$this` is pre-escaped
    // by construction (ADR-0036), so even a copy would have dropped the non-readonly
    // prop — the two roads agree, and the silence is the escape rule, not an omission.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        final class Box {\n\
        \x20 public mixed $value = null;\n\
        \x20 public function outer(): void { $this->value = 1; $this->inner(); }\n\
        \x20 public function inner(): void { needString($this->value); }\n\
        }\n";
    assert_eq!(count(src), 0, "a `$this`-origin receiver carries no non-readonly prop");

    // Written through the *variable* instead, so the receiver is an exact
    // `Receiver::Var` — the same two lines, the other road, and the prop crosses.
    let via_var = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        final class Box {\n\
        \x20 public mixed $value = null;\n\
        \x20 public function inner(): void { needString($this->value); }\n\
        }\n$b = new Box();\n$b->value = 1;\n$b->inner();\n";
    let f = findings(via_var);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn a_non_exact_receiver_seeds_as_today() {
    // A laundered `$this` alias is a lower bound (audit G1), so it resolves through
    // the final/private override guard, `body_this_exact` is `None`, and no copy is
    // admissible: the running instance may be a subclass whose props are not these.
    // `final` is what makes the guard admit the call at all, so the silence below is
    // the exactness bit and not a failure to resolve.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        final class Box {\n\
        \x20 public mixed $value = null;\n\
        \x20 public function inner(): void { needString($this->value); }\n\
        \x20 public function outer(): void { $this->value = 1; $that = $this; $that->inner(); }\n\
        }\n";
    assert_eq!(count(src), 0, "a non-exact receiver proves no object to copy");
}

#[test]
fn a_swept_receiver_carries_nothing_in() {
    // The caller-side sweep is untouched (ADR-0086 §2's refusal, inherited here): the
    // first call escapes `$b` and forgets its props, so the second call's copy has
    // nothing to carry. The receiver leg buys precision going in and pays for none
    // coming out.
    let src = format!("{BOXM}$b = new Box(1);\n$b->reset();\n$b->m();\n");
    assert_eq!(count(&src), 0, "the first call swept what the second would have read");

    // Without the intervening call the very same read fires — the silence above is
    // the sweep.
    assert_eq!(count(&format!("{BOXM}$b = new Box(1);\n$b->m();\n")), 1);

    // And the dump surface says the same thing about the return.
    assert_eq!(
        dumped(&format!("{BOXM}$b = new Box(1);\n$b->reset();\n\\PHPStan\\dumpType($b->get());")),
        "dumped type: unknown",
    );
}

#[test]
fn an_escaped_receiver_crosses_only_its_readonly_props() {
    // The field table is the argument leg's verbatim (ADR-0086 §3), escape rule
    // included: an object some other name can reach keeps its non-readonly props to
    // itself, and its readonly ones cross regardless — which is what makes the
    // silence attributable to the escape rather than to the seed failing wholesale.
    let src = "<?php\ndeclare(strict_types=1);\n\
        function needString(string $s): void {}\n\
        class Slot { public mixed $held = null; }\n\
        class Pair {\n\
        \x20 public mixed $value = null;\n\
        \x20 public function __construct(public readonly int $ro) {}\n\
        \x20 public function m(): void { needString($this->value); needString($this->ro); }\n\
        }\n$s = new Slot();\n$p = new Pair(7);\n$s->held = $p;\n$p->value = 1;\n$p->m();\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "only the readonly prop crossed: {f:#?}");
    assert_eq!(f[0].id, ID);
    assert!(f[0].message.contains('7'), "the finding is the readonly 7: {f:#?}");

    // Drop the escape and both props cross.
    let local = src.replace("$s->held = $p;\n", "");
    assert_eq!(count(&local), 2, "an unescaped receiver crosses both props");
}

#[test]
fn the_this_key_distinguishes_two_receivers() {
    // ADR-0075 §2.1's memo, under the entry state the receiver leg added: a `this:`
    // key naming only the class would let `$b1->m()`'s walk answer for `$b2->m()` —
    // replaying the first summary and suppressing the second's emission. Each
    // receiver's own state is in the key, so the first fires and the second does not.
    let src = format!("{BOXM}$b1 = new Box(1);\n$b1->m();\n$b2 = new Box('s');\n$b2->m();\n");
    let f = findings(&src);
    assert_eq!(f.len(), 1, "exactly the int receiver fires: {f:#?}");
    assert_eq!(f[0].id, ID);

    // Reversed, so the answer follows the receiver rather than the call order.
    let rev = format!("{BOXM}$b1 = new Box('s');\n$b1->m();\n$b2 = new Box(1);\n$b2->m();\n");
    assert_eq!(count(&rev), 1);

    // The same receiver state twice on one line is one finding: same key, same site.
    let twice = format!("{BOXM}$b1 = new Box(1); $b2 = new Box(1); $b1->m(); $b2->m();\n");
    assert_eq!(count(&twice), 1, "one key, one emission");
}

// By-ref parameters keep their decline.

#[test]
fn a_by_ref_parameter_refuses_the_whole_descent() {
    // The descent has always declined a by-ref parameter wholesale; the object seed
    // inherits that decline rather than sneaking a copy past it (ADR-0086 §4).
    let src = format!(
        "{PRELUDE}function f(Box &$b): void {{ needString($b->value); }}\n\
         $b = new Box(1);\nf($b);\n"
    );
    assert_eq!(count(&src), 0, "a by-ref parameter binds nothing, object or value");
}
