//! Object state (ADR-0036): the allocation-keyed heap, aliasing, `clone` isolation,
//! escape sets and sweeps, `readonly` immunity, and the property checks
//! (`type.property-mismatch`, `phpdoc.property-mismatch`, `readonly.reassigned`).
//!
//! Property facts are observed via diagnostics: a bad value read into a native-typed
//! arg (`needInt($o->p)`) fires `type.argument-mismatch`, witnessing the heap kept it.

use steins_db::{Project, SourceFile, SteinsDatabase};
use steins_infer::{
    DEBUG_TYPE_ID, Diagnostic, ID, NoFold, PARAM_MISMATCH_ID, PHPDOC_PROP_MISMATCH_ID,
    PROP_MISMATCH_ID, READONLY_REASSIGNED_ID, check_project,
};

fn findings(src: &str) -> Vec<Diagnostic> {
    let db = SteinsDatabase::default();
    let input = SourceFile::new(&db, "main.php".to_owned(), src.to_owned());
    let project = Project::new(&db, vec![input], steins_db::ProjectLayout::fallback(), steins_db::PluginFacts::none());
    // `untyped.*` (ADR-0078, #200) reports on the fixtures' own deliberately-untyped
    // declarations, not the behaviour under test — dropped to keep assertions stable.
    check_project(&db, project, &mut NoFold)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

fn count(src: &str) -> usize {
    findings(src).len()
}

/// A prelude shared by most tests: an `int`-typed sink and an untyped-property box.
const PRELUDE: &str = "<?php\nfunction needInt(int $x): int { return $x; }\nclass Box { public $p; }\n";

// Aliasing: writes visible through every alias

#[test]
fn alias_write_visible_via_original() {
    let src = format!("{PRELUDE}$a = new Box();\n$a->p = \"abc\";\n$b = $a;\nneedInt($b->p);\n");
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn alias_write_visible_via_alias() {
    let src = format!("{PRELUDE}$a = new Box();\n$b = $a;\n$b->p = \"abc\";\nneedInt($a->p);\n");
    assert_eq!(count(&src), 1);
}

// clone isolation (adversarial #1), both directions

#[test]
fn clone_isolates_both_directions() {
    // Corrupt only the clone: original stays clean (no finding for `$a->p`), clone
    // carries bad value (one for `$c->p`) — naive id-sharing would misfire on both.
    let src = format!(
        "{PRELUDE}$a = new Box();\n$a->p = 5;\n$c = clone $a;\n$c->p = \"abc\";\nneedInt($a->p);\nneedInt($c->p);\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
    assert_eq!(f[0].line, 9, "finding is on the $c->p read, not $a->p: {f:#?}");
}

#[test]
fn clone_write_does_not_leak_to_original() {
    let src = format!(
        "{PRELUDE}$a = new Box();\n$a->p = 5;\n$c = clone $a;\n$c->p = \"abc\";\nneedInt($a->p);\n"
    );
    assert_eq!(count(&src), 0);
}

#[test]
fn self_clone_does_not_panic_and_preserves_props() {
    // Self-clone (`var == src`): `clone $a` evaluates against the object, then assigns
    // back to `$a`. Panicked (`bound var has an id`) when the clone arm unbound `$a`
    // first (mautic `$dateFrom = clone $dateFrom`); sound: fresh copy, clean stays clean.
    let src = format!("{PRELUDE}$a = new Box();\n$a->p = 5;\n$a = clone $a;\nneedInt($a->p);\n");
    assert_eq!(count(&src), 0, "self-clone must preserve the good prop fact");
}

#[test]
fn self_clone_yields_writable_isolated_copy() {
    // Self-clone result is live and writable: corrupting its prop after fires (one
    // finding), proving the assignment rebound `$a` rather than degrading to Opaque.
    let src = format!(
        "{PRELUDE}$a = new Box();\n$a->p = 5;\n$a = clone $a;\n$a->p = \"abc\";\nneedInt($a->p);\n"
    );
    assert_eq!(count(&src), 1, "corrupted prop on the self-cloned object must fire");
}

// Escape sweep on pass-to-unknown; non-escaped survival (the payoff)

#[test]
fn escape_sweep_on_pass_to_unknown() {
    // `$a`/`$b` alias passed to unknown: props swept — a real sweep, not just binding loss.
    let src = format!(
        "{PRELUDE}$a = new Box();\n$a->p = \"abc\";\n$b = $a;\nsink($a);\nneedInt($b->p);\n"
    );
    assert_eq!(count(&src), 0, "escaped object's props must die");
}

#[test]
fn non_escaped_survives_unknown_call() {
    // Payoff: a purely-local object keeps its facts across an unrelated unknown call.
    let src = format!("{PRELUDE}$a = new Box();\n$a->p = \"abc\";\nunknownFn();\nneedInt($a->p);\n");
    assert_eq!(count(&src), 1, "non-escaped object's props must survive");
}

// Other escape triggers: prop-store, closure capture

#[test]
fn store_into_property_escapes() {
    let src = format!(
        "{PRELUDE}class Holder {{ public $item; }}\n$h = new Holder();\n$a = new Box();\n$a->p = \"abc\";\n$b = $a;\n$h->item = $a;\nunknownFn();\nneedInt($b->p);\n"
    );
    assert_eq!(count(&src), 0, "an object stored into a property escapes and is swept");
}

#[test]
fn closure_capture_escapes() {
    let src = format!(
        "{PRELUDE}$a = new Box();\n$a->p = \"abc\";\n$b = $a;\n$f = function() use ($a) {{ return $a; }};\nunknownFn();\nneedInt($b->p);\n"
    );
    assert_eq!(count(&src), 0, "a captured object escapes and is swept");
}

// $this: every call that runs with the SAME `$this` sweeps it

#[test]
fn this_is_swept_by_any_same_this_call_resolved_or_not() {
    // Amended 2026-08-16 (ADR-0057 C5): a *resolved* private or final `$this->m()`
    // used to sweep nothing, on the reasoning that the descent proves the target. It
    // does not prove the target's property writes — a descent into it seeds its own
    // `$this` copy (ADR-0086 §3), so `private function helper() { $this->p = 5; }`
    // would leave the caller's `"abc"` standing and convict correct code. Both runs
    // are silent now; the private call is no longer the precision payoff it read as.
    let src = "<?php\nfunction needInt(int $x): int { return $x; }\n\
class Widget {\n\
  public $p;\n\
  private function helper(): void {}\n\
  public function pub(): void {}\n\
  public function run(): void { $this->p = \"abc\"; $this->helper(); needInt($this->p); }\n\
  public function run2(): void { $this->p = \"abc\"; $this->pub(); needInt($this->p); }\n\
}\n";
    let f = findings(src);
    assert_eq!(f.len(), 0, "{f:#?}");

    // The write still stands where nothing runs with the same `$this`: an unrelated
    // *resolved* free function reaches no property of it.
    let unrelated = "<?php\nfunction needInt(int $x): int { return $x; }\n\
function noop(): void {}\n\
class W2 {\n\
  public $p;\n\
  public function run(): void { $this->p = \"abc\"; noop(); needInt($this->p); }\n\
}\n";
    let f = findings(unrelated);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

// readonly immunity: persists through escape + unknown calls

#[test]
fn readonly_persists_through_escape_and_unknown_call() {
    let src = "<?php\nfunction needInt(int $x): int { return $x; }\n\
class Ro { public function __construct(public readonly string $name) {} }\n\
$r = new Ro(\"abc\");\n$alias = $r;\nsink($r);\nunknownFn();\nneedInt($alias->name);\n";
    // Readonly `$name` survives the escape+sweep, so the bad string flows into needInt().
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

// readonly.reassigned

#[test]
fn readonly_reassigned_fires_on_proven_double_write() {
    // Promoted param is the (proven) first write; the method write is the second.
    let src = "<?php\n\
class Acct {\n\
  public function __construct(public readonly int $balance) {}\n\
  public function reset(): void { $this->balance = 0; }\n\
}\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, READONLY_REASSIGNED_ID);
    assert!(f[0].message.contains("readonly property"), "{}", f[0].message);
}

#[test]
fn readonly_reassigned_silent_when_second_write_conditional() {
    let src = "<?php\n\
class Acct {\n\
  public function __construct(public readonly int $balance) {}\n\
  public function reset(bool $c): void { if ($c) { $this->balance = 0; } }\n\
}\n";
    assert_eq!(count(src), 0, "a conditional second write is not a proven reassignment");
}

// type.property-mismatch (strict / coercive / silent-on-unknown)

#[test]
fn property_mismatch_coercive_nonnumeric_string() {
    let src = "<?php\nclass T { public int $n = 0; }\n$t = new T();\n$t->n = \"abc\";\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, PROP_MISMATCH_ID);
    assert!(f[0].message.contains("property"), "{}", f[0].message);
}

#[test]
fn property_mismatch_coercive_silent_on_numeric_string() {
    // Coercive mode: "5" coerces into int — no finding.
    let src = "<?php\nclass T { public int $n = 0; }\n$t = new T();\n$t->n = \"5\";\n";
    assert_eq!(count(src), 0);
}

#[test]
fn property_mismatch_strict_fires_on_numeric_string() {
    let src = "<?php\ndeclare(strict_types=1);\nclass T { public int $n = 0; }\n$t = new T();\n$t->n = \"5\";\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, PROP_MISMATCH_ID);
}

#[test]
fn property_mismatch_silent_on_unknown_value() {
    let src = "<?php\nclass T { public int $n = 0; }\n$t = new T();\n$t->n = someUnknown();\n";
    assert_eq!(count(src), 0);
}

// phpdoc.property-mismatch (@var contract, incl. abstract fact)

#[test]
fn phpdoc_property_mismatch_proven_value() {
    let src = "<?php\nclass P {\n  /** @var non-empty-string */\n  public $tag = \"x\";\n}\n$p = new P();\n$p->tag = \"\";\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, PHPDOC_PROP_MISMATCH_ID);
}

#[test]
fn phpdoc_property_mismatch_abstract_fact() {
    // `$s` is only known to be a string (abstract fact); `@var int` definitely rejects it.
    let src = "<?php\nclass P2 {\n  /** @var int */\n  public $num = 0;\n}\n\
function set(string $s): void { $p = new P2(); $p->num = $s; }\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, PHPDOC_PROP_MISMATCH_ID);
}

// Property read flows into a width()-style argument check

#[test]
fn prop_read_flows_into_arg_check() {
    let src = format!("{PRELUDE}$b = new Box();\n$b->p = \"abc\";\nneedInt($b->p);\n");
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

// Promoted-param: no double-report

#[test]
fn promoted_param_no_double_report() {
    // `new Pt("abc")` is a single ctor-argument violation, not also a property one.
    let src = "<?php\nclass Pt { public function __construct(public int $x) {} }\n$p = new Pt(\"abc\");\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID, "the single finding is the ctor-arg check, not a property one");
}

// Property hooks (PHP 8.4): a hooked prop binds no value fact (FP class 16)

#[test]
fn hooked_promoted_binds_no_fact() {
    // A `set` hook routes the write through arbitrary code, so reading must yield
    // Unknown (before the fix, raw "abc" bound and fired a false `type.argument-mismatch`).
    let src = format!(
        "{PRELUDE}class Hk {{ public function __construct(public $n {{ set {{ $this->n = $value; }} }}) {{}} }}\n$h = new Hk(\"abc\");\nneedInt($h->n);\n"
    );
    assert_eq!(count(&src), 0, "a hooked promoted prop binds no fact — no false finding");
}

#[test]
fn hooked_promoted_later_assign_binds_no_fact() {
    // Later `$h->n = "abc"` on a hooked prop also runs `set` — still no fact bound.
    let src = format!(
        "{PRELUDE}class Hk {{ public function __construct(public $n {{ set {{ $this->n = $value; }} }}) {{}} }}\n$h = new Hk(1);\n$h->n = \"abc\";\nneedInt($h->n);\n"
    );
    assert_eq!(count(&src), 0, "assignment through a set hook binds no fact");
}

#[test]
fn unhooked_promoted_still_binds_positional_and_named() {
    // Regression guard: an unhooked promoted prop still binds via both positional
    // and named `new` args (commit 3c7461e); two proven-bad reads → two findings.
    let src = format!(
        "{PRELUDE}class Pl {{ public function __construct(public $n) {{}} }}\n$a = new Pl(\"abc\");\nneedInt($a->n);\n$b = new Pl(n: \"abc\");\nneedInt($b->n);\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 2, "unhooked promoted binds both positional and named: {f:#?}");
    assert!(f.iter().all(|d| d.id == ID), "both are argument-mismatch findings");
}

// Adversarial #2: by-ref property alias must not keep a stale fact

#[test]
fn by_ref_property_alias_poisons_and_avoids_stale_fact() {
    // `$r = &$b->p` aliases the property; `changeIt($r)` may rewrite it invisibly, so
    // poison (reference-assignment family) drops all facts, avoiding a false flag.
    let src = format!(
        "{PRELUDE}function changeIt(&$x): void {{ $x = 1; }}\n$b = new Box();\n$b->p = \"abc\";\n$r = &$b->p;\nchangeIt($r);\nneedInt($b->p);\n"
    );
    assert_eq!(count(&src), 0, "poison must prevent a stale property fact from firing");
}

// A literal default the constructor overwrites (ADR-0086 §4, the stale-default half)

/// The single dump a fixture asks for.
fn dumped(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src).into_iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ds.len(), 1, "expected exactly one dump, got {ds:?}");
    ds[0].message.clone()
}

/// The shape a private corpus caught: two untyped properties with literal defaults,
/// both overwritten in the constructor **body** — which `build_new_object` never
/// walks. Seeding the defaults anyway put a proven `0` on a freshly allocated object.
const OVERWRITTEN: &str = "<?php\nfinal class H {\n\
    \x20 private $ad_count = 0;\n\
    \x20 private $view = 0;\n\
    \x20 public function __construct($original_view, $noAds = false) {\n\
    \x20   $this->ad_count = 1;\n\
    \x20   if (SomeUnknown::flag()) { $this->ad_count = 0; }\n\
    \x20   $this->view = $original_view - $this->ad_count;\n\
    \x20 }\n\
    \x20 public function getView() { return $this->view; }\n\
    }\n";

#[test]
fn a_default_the_constructor_overwrites_is_not_the_constructed_value() {
    // Read straight off the caller's object: `$this->view = $original_view - …` runs
    // before anyone can look, so `0` was never the value — only what the slot held
    // for the instant between the allocation and the first statement.
    assert_eq!(
        dumped(&format!("{OVERWRITTEN}$h = new H(7, false);\n\\PHPStan\\dumpType($h->view);")),
        "dumped type: unknown",
    );
    // And through the receiver leg's `$this` copy (ADR-0086 §3), which is the route
    // that turned a stale heap fact into a *finding* — a summary the caller rebinds.
    assert_eq!(
        dumped(&format!(
            "{OVERWRITTEN}$h = new H(7, false);\n$v = $h->getView();\n\\PHPStan\\dumpType($v);"
        )),
        "dumped type: unknown",
    );
    // The finding itself: a declared `positive-int` convicted correct code, because
    // `0` is not positive and `0` was not the value.
    let convict = format!(
        "{OVERWRITTEN}/** @param positive-int $n */\nfunction perPage(int $t, $n): int {{ return 1; }}\n\
         $h = new H(7, false);\n$v = $h->getView();\nperPage(10, $v);\n"
    );
    let f = findings(&convict);
    assert_eq!(
        f.iter().filter(|d| d.id == PARAM_MISMATCH_ID).count(),
        0,
        "a default the constructor overwrites premises nothing: {f:#?}",
    );
}

#[test]
fn a_default_the_constructor_leaves_alone_still_seeds() {
    // The gate is per property, not per class: `$b` is never spelled in the body, so
    // its default is still the constructed value and still fires at a typed sink.
    let src = format!(
        "{PRELUDE}class D {{\n\
         \x20 public $a = \"x\";\n\
         \x20 public $b = \"y\";\n\
         \x20 public function __construct() {{ $this->a = 1; }}\n\
         }}\n$d = new D();\nneedInt($d->b);\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 1, "the untouched default crossed: {f:#?}");
    assert_eq!(f[0].id, ID);

    // Its touched sibling does not, which is what makes the line above the gate's
    // shape rather than a wholesale drop.
    let touched = src.replace("needInt($d->b)", "needInt($d->a)");
    assert_eq!(count(&touched), 0);

    // A mention that is not a write drops the seed at the FLOOR — the scan asks
    // whether the body can refer to the slot, not what it does with it. Where the
    // constructor is **walked** (ADR-0057's constructor-summary amendment, C1) the
    // over-approximation is not needed and not used: the walk runs `echo $this->a;`,
    // writes nothing, and the default is the constructed value.
    let read_only = format!(
        "{PRELUDE}class R {{\n\
         \x20 public $a = \"x\";\n\
         \x20 public function __construct() {{ echo $this->a; }}\n\
         }}\n$r = new R();\nneedInt($r->a);\n"
    );
    let f = findings(&read_only);
    assert_eq!(f.len(), 1, "a read is not a write, and the walk knows it: {f:#?}");
    assert_eq!(f[0].id, ID);

    // And a longer name that merely starts with the property's is not the property:
    // `$this->ab` must not drop `$a`'s default (token boundaries, `mentions_variable`
    // discipline).
    let prefix = format!(
        "{PRELUDE}class Pf {{\n\
         \x20 public $a = \"x\";\n\
         \x20 public $ab = 1;\n\
         \x20 public function __construct() {{ $this->ab = 2; }}\n\
         }}\n$p = new Pf();\nneedInt($p->a);\n"
    );
    let f = findings(&prefix);
    assert_eq!(f.len(), 1, "`$this->ab` is not a mention of `$a`: {f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn a_class_with_no_constructor_keeps_its_defaults() {
    // Nothing runs between the allocation and the first read, so the declared default
    // IS the constructed value — the case the gate must not touch.
    let src =
        format!("{PRELUDE}class N {{ public $n = \"abc\"; }}\n$n = new N();\nneedInt($n->n);\n");
    let f = findings(&src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
    assert_eq!(
        dumped(
            "<?php\nclass N2 { public $n = 5; }\n$n = new N2();\n\\PHPStan\\dumpType($n->n);"
        ),
        "dumped type: 5",
    );
}

#[test]
fn an_inherited_constructor_drops_the_childs_default_too() {
    // `find_ctor` walks the chain, so the constructor that RUNS is the parent's, and
    // it is the parent's body text the scan reads — the child's declaration is where
    // the default sits and the parent's is where it is overwritten.
    let src = format!(
        "{PRELUDE}class Par {{ public function __construct() {{ $this->view = 1; }} }}\n\
         class Ch extends Par {{ public $view = \"abc\"; }}\n$c = new Ch();\nneedInt($c->view);\n"
    );
    assert_eq!(count(&src), 0, "the inherited constructor writes it, so no default crosses");

    // The same pair with a property the parent's constructor never names.
    let untouched = src.replace("$this->view = 1;", "$this->other = 1;");
    let f = findings(&untouched);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn a_dynamic_property_write_in_the_constructor_drops_every_default() {
    // `$this->$name = …` writes a slot without spelling it — the property-world twin
    // of `extract()`, except it does NOT poison the scope, so nothing but this clause
    // would catch it. No slot is safe, so no default survives.
    let src = format!(
        "{PRELUDE}class Dy {{\n\
         \x20 public $a = \"abc\";\n\
         \x20 public function __construct(string $name) {{ $this->$name = 1; }}\n\
         }}\n$d = new Dy(\"a\");\nneedInt($d->a);\n"
    );
    assert_eq!(count(&src), 0, "a dynamic write names no slot, so none is safe");

    // The `{…}` spelling likewise.
    let braced = src.replace("$this->$name = 1;", "$this->{$name} = 1;");
    assert_eq!(count(&braced), 0);

    // And a static write to an unrelated slot in the same constructor leaves `$a`
    // alone — the drop above is the dynamism, not the constructor's existence.
    let statik = src.replace("$this->$name = 1;", "$this->b = 1;");
    let f = findings(&statik);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, ID);
}

#[test]
fn the_arrow_may_be_split_across_lines() {
    // Whitespace of any kind sits around the arrow: `$this` and its `->` on two lines
    // is one access, and a scan that skipped only spaces and tabs would miss it and
    // keep a wrong default. The failure direction is what makes this worth a fixture.
    let src = format!(
        "{PRELUDE}class Ml {{\n\
         \x20 public $a = \"abc\";\n\
         \x20 public function __construct() {{\n\
         \x20   $this\n\
         \x20     ->a = 1;\n\
         \x20 }}\n\
         }}\n$m = new Ml();\nneedInt($m->a);\n"
    );
    assert_eq!(count(&src), 0, "a split arrow is still a mention");
}

#[test]
fn a_constructor_that_lets_this_out_of_its_own_text_drops_every_default() {
    // The per-property rule can only speak about slots the text spells. Four shapes
    // reach a slot without spelling it, and each drops EVERY default — deliberately
    // coarse, because a wrong `Verified` default is a proof-layer false positive and
    // a dropped one is only lost knowledge (ADR-0086 §4).

    // (b) Delegation: `init()` writes `$this->a`, and the constructor's own text
    // never says so.
    let delegating = format!(
        "{PRELUDE}class De {{\n\
         \x20 public $a = \"abc\";\n\
         \x20 public function __construct() {{ $this->init(); }}\n\
         \x20 private function init(): void {{ $this->a = 1; }}\n\
         }}\n$d = new De();\nneedInt($d->a);\n"
    );
    assert_eq!(count(&delegating), 0, "a `$this` method call reaches any slot");

    // (c) The same shape spelled `parent::__construct()`, which runs with this very
    // `$this`. The parent writes the child's declared slot.
    let inherited = format!(
        "{PRELUDE}class Ba {{ public function __construct() {{ $this->a = 1; }} }}\n\
         class Ch2 extends Ba {{\n\
         \x20 public $a = \"abc\";\n\
         \x20 public function __construct() {{ parent::__construct(); }}\n\
         }}\n$c = new Ch2();\nneedInt($c->a);\n"
    );
    assert_eq!(count(&inherited), 0, "`parent::__construct()` reaches any slot");

    // `self::` and `static::` are the same argument; a bare `self::CONST` is not a
    // call and runs nothing, so it must NOT drop anything.
    let selfcall = inherited
        .replace("parent::__construct();", "self::init();")
        .replace(
            "public $a = \"abc\";",
            "public $a = \"abc\";\n private function init(): void { $this->a = 1; }",
        );
    assert_eq!(count(&selfcall), 0, "`self::m()` reaches any slot");

    let const_read = format!(
        "{PRELUDE}class Co {{\n\
         \x20 const K = 1;\n\
         \x20 public $a = \"abc\";\n\
         \x20 public function __construct() {{ $n = self::K; }}\n\
         }}\n$c = new Co();\nneedInt($c->a);\n"
    );
    let f = findings(&const_read);
    assert_eq!(f.len(), 1, "`self::K` runs nothing, so the default stands: {f:#?}");
    assert_eq!(f[0].id, ID);

    // (a) A bare `$this` handed to a free function: an alias leaves the text, and any
    // callee can write through it.
    let passed = format!(
        "{PRELUDE}function register($o): void {{}}\n\
         class Pa {{\n\
         \x20 public $a = \"abc\";\n\
         \x20 public function __construct() {{ register($this); }}\n\
         }}\n$p = new Pa();\nneedInt($p->a);\n"
    );
    assert_eq!(count(&passed), 0, "a bare `$this` hands out an alias");

    // Two shapes the FLOOR over-approximates and the WALK decides, each time in the
    // direction the runtime agrees with (ADR-0057 C1 — a walked body needs no
    // over-approximation of itself). An alias that never leaves the constructor, and
    // a closure binding `$this` that is never invoked, both write nothing, so `$a`
    // really IS `"abc"` when `new Pa()` returns.
    let aliased = passed.replace("register($this);", "$that = $this;");
    let f = findings(&aliased);
    assert_eq!(f.len(), 1, "an alias that goes nowhere writes nothing: {f:#?}");
    assert_eq!(f[0].id, ID);
    let captured = passed.replace("register($this);", "$f = function () { $this->a = 1; };");
    let f = findings(&captured);
    assert_eq!(f.len(), 1, "an uninvoked closure writes nothing: {f:#?}");
    assert_eq!(f[0].id, ID);

    // Let either one actually go somewhere and the answer flips back: handing the
    // alias to a call escapes the shared allocation, and invoking the closure is an
    // unresolved call, which sweeps the constructor's own unescaped `$this` (C5).
    let alias_passed =
        passed.replace("register($this);", "$that = $this; register($that);");
    assert_eq!(count(&alias_passed), 0, "the alias escapes the same allocation");
    let invoked =
        passed.replace("register($this);", "$f = function () { $this->a = 1; }; $f();");
    let f = findings(&invoked);
    assert_eq!(f.len(), 0, "an unresolved call sweeps the constructor's `$this`: {f:#?}");
}

#[test]
fn a_constructor_that_only_writes_its_own_slots_stays_fine_grained() {
    // The coarse rule is a fallback, not the rule: where nothing delegates, the
    // per-property scan still distinguishes the slots the body names from the ones it
    // does not. `$c` keeps its default; `$a` and `$b` do not.
    let src = format!(
        "{PRELUDE}class Fg {{\n\
         \x20 public $a = 1;\n\
         \x20 public $b = 1;\n\
         \x20 public $c = \"abc\";\n\
         \x20 public function __construct($x) {{ $this->a = 1; $this->b = $x; }}\n\
         }}\n$g = new Fg(2);\nneedInt($g->c);\n"
    );
    let f = findings(&src);
    assert_eq!(f.len(), 1, "`$c` was never spelled, so its default stands: {f:#?}");
    assert_eq!(f[0].id, ID);

    // And the two it does spell are unknown, which is what makes the line above a
    // statement about `$c` rather than about the constructor.
    let spelled = src.replace("needInt($g->c)", "needInt($g->a)");
    assert_eq!(count(&spelled), 0);
}

#[test]
fn a_poisoned_constructor_seeds_no_defaults_at_all() {
    // `extract()` can write a slot without spelling it, so a lexical scan proves
    // nothing there. Unknown is never proof that a default stands.
    let src = format!(
        "{PRELUDE}class P3 {{\n\
         \x20 public $a = \"abc\";\n\
         \x20 public function __construct(array $vals) {{ extract($vals); }}\n\
         }}\n$p = new P3([]);\nneedInt($p->a);\n"
    );
    assert_eq!(count(&src), 0, "a poisoned constructor withholds every default");
}

#[test]
fn readonly_bookkeeping_is_untouched_by_the_gate() {
    // The gate drops a *value*, never the record that the slot was written: a promoted
    // readonly prop is written at construction, so a later write is still a reassign.
    // (PHP forbids a default on a readonly property outright, so the gate's clause and
    // the readonly clause cannot even meet — pinned so a refactor cannot merge them.)
    let src = "<?php\nclass Ro {\n\
        \x20 public $touched = 0;\n\
        \x20 public function __construct(public readonly int $id) { $this->touched = 1; }\n\
        }\n$r = new Ro(1);\n$r->id = 2;\n";
    let f = findings(src);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].id, READONLY_REASSIGNED_ID);
}
