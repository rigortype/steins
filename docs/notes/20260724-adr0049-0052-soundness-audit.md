# Soundness audit: ADR-0049 absence family + ADR-0052 narrowing (pre-implementation, gates S2/S4/S6/N4)

Read-only design audit, 2026-07-24. Scope: the finding-breadth ladders
(ADR-0049 §2–§8) and their narrowing dependency (ADR-0052 §2/§3/§5),
cross-checked against the landed machinery (`is_a` oracle, project index,
`DynamismSite` lowering, allocation-keyed heap, trait handling,
class-like lowering, transform-side dam). Every concern below carries a
counterexample or a code citation; "verify against PHP 8.5" marks the
places where a runtime behavior was not re-verified here.

Line numbers cite the tree at commit `2be9a14`.

---

## Part A — Genuine gaps (design change or new silence leg required)

### G1 (highest priority, LIVE): the heap `class` field is a lower bound for `$this` but every consumer reads it as exactness

**(a) Ladder/rule.** ADR-0049 §4a ("receiver class **proven** — heap
exact class (ADR-0036)"), and — already landed — the ADR-0043 stage-3
object acceptance definite-No and `eval_instanceof`.

**(b) The leg that is incomplete-while-believed-complete.**
`seed_this_object` (`crates/steins-infer/src/lib.rs:3438`) binds `$this`
to a `HeapObj { class: <enclosing class>, … }` whenever the class has
tracked properties. `HeapObj` has **no exactness bit**: for a
`new Foo` allocation the class is exact; for the `$this` seed it is only
a lower bound (the runtime object may be any descendant). All four
consumers of `Store::class_of` read it as exact:

- `lib.rs:5084` — object acceptance definite-No on arguments;
- `lib.rs:4148` (`eval_instanceof`) — branch verdicts;
- `lib.rs:5545` (`resolve_call_target`, `Receiver::Var`) —
  `resolve_exact` **without** the override guard;
- `lib.rs:6333` (`resolve_cval`) — `CVal::Object` for phpdoc acceptance.

The dispatch layer itself knows `$this` is not exact
(`Receiver::This` routes through `resolve_guarded`'s final/private
guard, `lib.rs:5548-5553`), but the knowledge lives in the *receiver
syntax*, not in the heap fact — so `$u = $this;` (plain aliasing,
`lib.rs:3310-3314`) and `clone $this` (`lib.rs:3317`) launder the
enclosing class into an "exact" `Receiver::Var`. And `$this` used
directly as an *argument* or *instanceof operand* never passes through
the dispatch guard at all.

**(c) Counterexamples.** Live today (13th FP class, not hypothetical —
the emitting path is `lib.rs:5080-5096` with
`member_rejects_object` at `lib.rs:6407`):

```php
<?php
class Node2 {
    public int $depth = 0;              // any tracked prop → $this gets heap-seeded
    public function register(): void {
        add_leaf($this);                // REPORTED: $this "holds a Node2", Leaf rejects
    }
}
class Leaf extends Node2 {}
function add_leaf(Leaf $l): void {}
(new Leaf())->register();               // runtime: fine — $this IS a Leaf
```

`is_a(Node2, Leaf)` walks Node2's (empty, complete) ancestor set →
`No` → every union member rejects → definite-No diagnostic. Runtime is
clean.

Second live consequence — wrong branch death (`eval_instanceof`
returns `Certainty::No`, and `walk_if` at `lib.rs:3717` then **skips
the then-branch entirely**, so the post-if env carries else-only facts
as definite):

```php
<?php
class Base {
    public int $x = 0;
    public function f(): void {
        $v = 1;
        if ($this instanceof Sub) { $v = "hello"; }   // branch believed dead
        takes_string($v);   // checker: $v is Singleton(1) → definite type error
    }
}
class Sub extends Base {}
function takes_string(string $s): void {}
(new Sub())->f();           // runtime: $v === "hello", fine
```

Future consequence if §4 ships against this store: the classic
template-method FP —

```php
<?php
class Handler {
    public int $n = 0;
    public function run(): void { $u = $this; $u->handle(); }
    // or directly: $this->handle();
}
class MailHandler extends Handler {
    public function handle(): void {}
}
(new MailHandler())->run();   // §4 as written: "undefined method Handler::handle()" — FP
```

(The direct `$this->handle()` form is guarded today only by the
*dispatch* code path; ADR-0049 §4 never mentions `$this` receivers, so
nothing stops the S2 emitter from consuming `class_of("this")`.)

**(d) Does the ADR address it?** No. §4a lists "heap exact class
(ADR-0036), `new`, or proven propagation" with no `$this` carve-out;
ADR-0036 describes `$this` only as *pre-escaped* (an aliasing/sweep
property, not an exactness property). ADR-0043's "instanceof binds no
exactness" note shows the right instinct but the seed bypasses it.

**(e) Mitigation.**
1. Add `class_exact: bool` to `HeapObj`. `new`/`clone`-of-exact ⇒ true;
   the `$this` seed ⇒ true only when descended with `this_exact`
   or the enclosing class is `final`; false otherwise.
2. Every exactness consumer (`eval_instanceof` Yes-side may keep
   membership Yes from a lower bound — `is_a(lower, T)=Yes` is still
   sound — but the **No side must require `class_exact`**; acceptance
   definite-No must require it; §4a must require it; `Receiver::Var`
   dispatch must fall back to the `resolve_guarded` discipline when
   inexact).
3. ADR-0049 §4 gains an explicit sentence: a `$this`-derived receiver
   is a membership fact, not exactness; method-absence on it needs the
   §8 descendant-closure ladder (which for `$this` includes the dam),
   or silence.
4. Fixtures: the three snippets above, pinned as silence tests before
   S2. The first two belong in the *current* fp-gate regardless of S2.

### G2: §4's dam-freedom conflates "cannot reopen a defined class" with "the indexed definition is the runtime definition"

**(a)** ADR-0049 §2/§4 — "PHP cannot reopen a defined class — no
runtime construct adds a method to a class the index resolved — so
*method*-absence claims need no dam at all."

**(b)** The immunity sentence is true of a *bound* class, but the
ladder's method list comes from the **indexed textual declaration**, and
nothing checks that this declaration is the one the runtime bound.
Three concrete ways the binding diverges while the index believes it is
unique and complete:

1. **Conditional (guarded) declaration + dynamism.** The index collects
   declarations at any nesting depth (`lower_classes_into`,
   `steins-syntax/src/lib.rs:1810-1826`, recurses unconditionally) and
   `ClassDecl` carries **no conditionality flag** — a class declared
   under `if (!class_exists(…))` is indistinguishable from a top-level
   one, and a *single* such declaration is `Res::Unique`, not
   Ambiguous (`insert_unique`, `steins-infer/src/lib.rs:440`).
   ADR-0046's "definition havoc … absorbed by the resolution-ambiguous
   refusal on conditional/duplicate definitions" exists only on the
   *transform* side; the checker index has no such refusal. An `eval`
   or out-of-universe include that defines the name *first* wins the
   guard; the indexed body never executes; no redeclaration fatal ever
   fires. The dam site exists (eval) — but §4 deliberately ignores the
   dam.

   ```php
   <?php // shim.php (project) — fallback stub, never loaded in prod
   if (!class_exists('Legacy\Order')) {
       class Order { public function pay(): void {} }   // no cancel()
   }
   // bootstrap: include $compiledCachePath;  ← dam site; defines Legacy\Order WITH cancel()
   $o = new \Legacy\Order();
   $o->cancel();        // §4 as written: undefined method — runtime: fine
   ```

2. **Project/vendor declaration shadowing a loaded extension class.**
   Polyfill and stub packages (symfony/polyfill-*, IDE-helper and
   phpstorm-stub-style packages vendored as source per ADR-0015) declare
   root-namespace twins of builtin/extension classes. At runtime on a
   PHP where the extension is loaded, the autoloader never fires for the
   name and the textual twin is dead code — yet `find_class`
   (`lib.rs:2172`) returns the project declaration and never consults
   the catalog or sidecar when a project hit exists, so the chain is
   "fully project-defined" (§4f satisfied) with a method surface that
   may be a stale approximation of the real extension class. No dam
   site exists at all in this variant. (Function-absence §3 has both
   the "not builtin-shadowed" leg and the sidecar leg; class §5 has the
   sidecar leg; **§4 alone has no sidecar leg**.)

3. **Literal `class_alias` edge colliding with a textual declaration.**
   §2 says literal `class_alias` contributes alias edges; §5 says
   existence consults them. The arbitration rule when an alias edge and
   a textual `ClassDecl` claim the same FQN (the runtime-shim +
   IDE-stub pattern: `class_alias('New\Impl', 'Legacy\Name')` beside a
   stub `class Legacy\Name {…}`) is stated nowhere. If the textual decl
   wins identity, method-absence is judged against the stub while
   runtime binds `New\Impl`.

**(c)** Snippets above.

**(d)** Not addressed. §1 handles conditional definitions for
*existence* ("never Absent") but is silent on *identity*; §4f handles
builtin *ancestors*, not builtin *homonyms*; the alias/decl collision
rule is unstated.

**(e) Mitigations (all three are silence legs, cheap):**
1. Lowering records `conditional: bool` on `ClassDecl` (true when the
   declaration is nested under anything but a plain namespace/program
   node). Method-absence (and §8 descendant closure) over a chain
   containing a conditional declaration requires **dam clear or
   vouched** — i.e., dam-freedom is retained only for unconditional
   top-level declarations. This preserves the S2 core-yield prediction
   (legacy classes are overwhelmingly unconditional) while closing the
   guarded-stub hole.
2. New §4 leg: no chain class's FQN is known to the **sidecar** as a
   builtin/extension class-like (homonym check — one `reflect` call per
   chain class, cacheable). Until the sidecar reflect slice exists,
   ship S2 with the cheaper approximation: chain class's FQN present in
   the builtin hierarchy table ⇒ Unknown-silent.
3. Alias-edge/textual-decl collision on one FQN ⇒ `Ambiguous`
   (existence: present; identity: unresolved). Two literal alias edges
   for one name ⇒ likewise Ambiguous. State it in the S1 slice.

### G3: enum lowering leaves `methods` empty and `uses_traits` false — a time bomb armed by the future builtin-ancestor unlock

**(a)** ADR-0049 §4c/§4e (method absent from every chain class; no
trait use anywhere) and §8; ADR-0043 enum lowering.

**(b)** `lower_enum` (`steins-syntax/src/lib.rs:2022-2081`) returns
`methods: Vec::new()` *and* `uses_traits: false` unconditionally —
`ClassLikeMember::TraitUse` falls into the `_ => {}` arm, so even the
trait-use *marker* is lost for enums. Today every enum chain contains
the builtin `UnitEnum` (`ancestors_of`, `steins-infer/src/lib.rs:6583`),
so §4f ("every chain class project-defined") forces Unknown-silence and
nothing fires. **The moment the sidecar reflect slice (M2 gap 7)
enumerates builtin interfaces' surfaces, the enum chain becomes "fully
enumerated" with an empty method list**, and every enum method call —
plus the engine-provided `cases()`/`from()`/`tryFrom()`, which have *no
textual declaration anywhere* — is reported undefined.

**(c)**
```php
<?php
enum Suit: string implements HasLabel {
    use LabelFormatting;                 // uses_traits recorded FALSE
    case Hearts = 'h';
    public function label(): string { return 'x'; }   // lowered away
}
$s = Suit::Hearts;
$s->label();          // FP once UnitEnum's surface is sidecar-enumerated
Suit::cases();        // FP likewise — cases() exists in no source text
```

**(d)** Not addressed: ADR-0049 never mentions enum method bodies;
ADR-0043 defers enum method lowering "with the method-transform stage"
with no linkage to the absence family. The two deferrals are
individually safe and jointly unsound.

**(e)** Write the silence condition **now**, as a ladder leg with a
fixture: `is_enum && enum-methods-not-lowered ⇒ Unknown` for §4/§6/§8,
independent of what the chain enumeration says. Make complete enum
lowering (methods + TraitUse flag) a hard precondition of the M2
builtin-ancestor unlock, recorded in that slice's gate. Engine-provided
`cases`/`from`/`tryFrom` (and `UnitEnum::cases` arity) must come from
the sidecar surface, never from the textual index.

### G4: anonymous classes are invisible to the class index — §8's "project-wide descendant set" can never be complete as currently lowered

**(a)** ADR-0049 §8 descendant closure.

**(b)** `lower_classes_into` matches only
`Node::Class | Node::Interface | Node::Enum`
(`steins-syntax/src/lib.rs:1817-1821`); `Node::AnonymousClass` is never
collected (multiple walkers explicitly list it under "nested scopes — do
not descend", e.g. `lib.rs:2497`). A descendant enumeration built on
`tree.classes()` therefore believes itself complete while missing every
`new class extends T` / `new class implements I`.

**(c)**
```php
<?php
/** @param Report $r */
function render($r): void {
    if (!$r instanceof HtmlReport) {
        $r->toPlainText();   // §8: Report's descendant set "completely enumerated"
    }                        //     = {HtmlReport} → toPlainText undefined → FP
}
class Report { public function body(): string { return ''; } }
class HtmlReport extends Report { public function toPlainText(): string { return ''; } }
function makeAdhoc(): Report {
    return new class extends Report {
        public function toPlainText(): string { return 'adhoc'; }   // invisible
    };
}
render(makeAdhoc());   // runtime: fine
```

**(d)** Not addressed; §8's text assumes the descendant scan sees every
`extends`/`implements` edge.

**(e)** Cheapest sound fix for S6: lowering records anonymous-class
declarations as *edge-only* entries (parent/implements NameRefs, no
name, plus their `uses_traits`/method names for the closure check) —
or, v1, the mere presence of any anonymous class whose
extends/implements resolves to (or is Unknown against) a union member
forces Unknown. Additional descendant-scan legs while here: iterate
**declarations, not the deduped index** (both halves of an
Ambiguous-FQN pair count as potential descendants); match parent refs
**through literal `class_alias` edges** (`class B extends LegacyName;`
+ `class_alias('T','LegacyName')` makes B a descendant of T); include
`implements` edges, interface-extends edges, and enum `implements` when
the union member is an interface.

### G5: the include-path "benign" judgment ignores `include_path`/CWD resolution — the dam can under-dam

**(a)** ADR-0046 §2 / ADR-0049 §2 — the dam's "include whose path is
proven to resolve inside the analyzed universe" exemption; code:
`include_is_benign` / `resolve_from`
(`crates/steins-edit/src/obstacles.rs:222-243`), which S1 will inherit
for the checker-side dam.

**(b)** `resolve_from` resolves a bare relative literal
(`include 'lib/util.php';`) against the **including file's directory**
and declares the site benign if that file is in the universe. PHP's
actual runtime resolution for a relative path consults `include_path`
first and the calling script's CWD, with the including file's own
directory only as the final fallback (and a `./`-prefixed path skips
`include_path` and binds to **CWD**, not the file's directory) — verify
the exact precedence against PHP 8.5, but the CWD/`include_path`
dependence is certain. A same-named file that happens to sit next to
the including file makes the dam believe the universe closed while the
runtime loads an out-of-universe twin.

**(c)**
```php
<?php // src/boot.php ; a src/config.php exists in-universe → "benign"
include 'config.php';
// prod runs `php /srv/app/bin/run.php` with include_path=.:/etc/app —
// /etc/app/config.php (out of universe) wins and defines helpers the
// scan never sees → call.undefined-function fires on a defined function.
```

**(d)** Not addressed — the ADR-0046 text says "proven to resolve",
and the code's notion of "proven" is dir-relative only. (This is a
latent under-damming in the *landed transform-side* oracle too, not
just the future checker dam.)

**(e)** Only `IncludePath::DirRelative` (`__DIR__ . '…'`) and
**absolute** literals qualify as provable; a bare relative
`IncludePath::Literal` must be treated as `Unproven` (dam) — or benign
only under an opt-in `[runtime] include-path` pseudo-constant declaring
the boot truth (the ADR-0037 §2 pattern already used for
warning-handler and zend-assertions). One-line change in
`include_is_benign`; add a fixture with a bare relative include.

### G6: the sidecar existence oracle is SAPI-blind — `call.undefined-function` can fire on functions that exist only in the serving SAPI

**(a)** ADR-0049 §1 oracle (b) and §3 leg (b): "the sidecar answers
not-found for every candidate on the project's own PHP."

**(b)** "The project's own PHP" is not one function table. The sidecar
runs CLI; production runs FPM/Apache. `fastcgi_finish_request` exists
under FPM and **not** under CLI; `apache_*`/`litespeed_*` likewise;
additionally per-SAPI ini files commonly load different extension sets
(`php.ini-cli` vs `php.ini-fpm`). Every leg of §3 then passes — index
Absent, sidecar not-found, dam clear — on a call that is defined in
every environment that actually executes the code path.

**(c)**
```php
<?php
function respond(string $body): void {
    echo $body;
    fastcgi_finish_request();   // CLI sidecar: not found → §3 fires → FP under FPM
}
```

**(d)** Not addressed. §1's "loaded extensions" phrasing assumes one
boot surface; ADR-0004 ask-the-real-thing is satisfied to the letter
and still wrong, because there are two real things.

**(e)** (i) Coverage posture records the sidecar SAPI and the claim
downgrades to "on SAPI cli" honestly; (ii) a small, curated
SAPI-provided allowlist (`fastcgi_finish_request`, `apache_*`,
`litespeed_*`, …) is never-Absent regardless of sidecar answer; (iii)
optionally a `[runtime] sapi`/`extensions` pseudo-constant so the user
declares the serving surface and the sidecar can be asked to verify
via `php-fpm -i`-equivalent. The same reasoning applies to
`class.undefined` for extension classes loaded only in one SAPI's ini.

### G7: `offset.missing` needs read-*context* legs the ADR never states — isset-family and by-ref argument positions do not warn

**(a)** ADR-0049 §7 — "this read provably emits Undefined array key k".

**(b)** The severity table is per-*operation*, but the ladder is
written per-*value*. Contexts where a key-absent read emits **no**
warning: `isset($a[k])`, `empty($a[k])`, `$a[k] ?? $d`,
`array_key_exists`, `unset($a[k])`, write positions (`$a[k] = …`,
`$a[k][j] = …` autovivification, `&$a[k]`, `list()`/foreach-list
targets), and — the trap — **an argument position whose parameter is
by-ref**: `f($a[0])` with `function f(&$x)` autovivifies silently.
The ADR states "writes are silent" but a by-ref argument is only
knowable as write-like when the callee is *resolved*; an unresolved
callee makes the position undecidable.

**(c)**
```php
<?php
$rows = [];
collect($rows[0], 'x');                    // runtime: no warning (autovivified)
function collect(&$slot, string $v): void { $slot[] = $v; }
// A §7 emitter keyed on "read of Singleton([]) at key 0" reports a
// warning that never happens.
```

**(d)** Partially: "Writes are silent (writes create keys…)" covers
assignment targets; the isset-family and argument-position legs are
unstated.

**(e)** Enumerate the eligible read contexts as a whitelist (plain
rvalue read, non-by-ref argument of a *resolved* target, `foreach`
subject, return operand, …); everything else — including any argument
of an unresolved/builtin-unreflected callee (param lowering already
carries `by_ref`, `steins-syntax/src/lib.rs:291`) — is silent. One
silence fixture per excluded context (the §10 silence-matrix
discipline). Also verify against PHP 8.5 whether the read half of
`$a[0]++` / `$a[0] .= …` on a missing key warns before deciding
whether compound assignment is in the whitelist.

### G8: stratum leakage is live today, and the consumption rule needs a *derivation* clause, not just an emission clause

**(a)** ADR-0052 §5; ADR-0049 §7's reliance on it
(`assertions_assert_non_empty_list`).

**(b)** Two halves:

1. **Live**: `apply_assert_to_var` (`steins-infer/src/lib.rs:4583`,
   insert at 4608) binds an assert-tag fact whose `"asserted"`
   provenance is prose; the landed proof-layer definite-No emitters
   consume it indistinguishably. A lying `@phpstan-assert` can already
   premise a `type.*` finding. ADR-0052 admits this ("tightens the
   landed code") but sequences the fix as N2, order-free after N1.
2. **Design text**: the binding rule speaks of "every fact [a finding]
   consumed" — an emission-time check. It does not state that
   *derived* facts (folded arithmetic over an Asserted operand, an
   array literal containing an Asserted element, a branch **join** of
   a Verified and an Asserted arm, a heap prop written from an
   Asserted value) inherit `min(stratum of inputs)`. If the stratum
   bit lives only on the directly-bound `Known`, one derivation
   launders Asserted into Verified.

**(c)**
```php
<?php
/** @phpstan-assert-if-true int $x */
function looksInt(mixed $x): bool { return true; }   // liar
function f(mixed $x): void {
    if (looksInt($x)) {
        $pair = [$x, 99];        // derived Singleton array, element stratum lost
        takes_string($pair[0]);  // offset proof + acceptance definite-No on an
    }                            // Asserted-derived value → proof-layer FP
}
function takes_string(string $s): void {}
```

**(d)** Half-addressed: the consumption rule and the
replace-if-weaker tightening are committed; the derivation clause and
the N2-before-S* sequencing are not.

**(e)** (i) State in ADR-0052 §5: *stratum is propagated through every
fact constructor — fold results, composed arrays/heap writes, and joins
take the minimum of their inputs' strata*. (ii) Make N2 a hard
prerequisite of S2–S6 in the rollout table (S-slices consume env facts;
shipping any absence id against the un-stratified `Known` re-opens the
hole §5 exists to close). (iii) Fixture: the snippet above must stay
silent; plus a join fixture (Verified branch ⊔ Asserted branch ⇒
Asserted).

---

## Part B — Medium: verify or harden (cheap legs, real but narrower exposure)

### M1: catalog builtin-hierarchy version skew vs the running PHP

`builtin_class_supers` reads a table generated from **pinned** php-src
mining data (`steins-catalog/src/lib.rs:27-31, 353`). A project running
a different minor can disagree with the pinned edge set in both
directions. The dangerous direction for ADR-0052 §2's **positive**
branch: a running-PHP edge the catalog lacks makes
`is_a(M, T) = No-under-closure` where truth is Yes → a final arm is
wrongly deleted → wrongly narrowed receiver → `phpdoc.undefined-method`
FP. (Negative-branch deletion needs a Yes, which a *stale-present* edge
could also fake.) The catalog already shows the right instinct —
builtin enums are deliberately absent because their edge sets would be
incomplete — extend it: when the sidecar-reported PHP minor differs
from the mining pin, demote catalog-backed `No` (and Yes-edges used for
arm deletion) to Unknown, or regenerate the table per supported minor
and select by sidecar version. `class.undefined` is already
double-locked by its sidecar leg; the narrowing arms are not.

### M2: `namespace\foo()` relative references have no `RefKind` and will mis-resolve

`RefKind` is FullyQualified/Qualified/Unqualified
(`steins-syntax/src/lib.rs:84-94`); PHP's relative form
`namespace\foo()` / `new namespace\Bar` would lower (via mago's
Qualified identifier) to raw `"namespace\foo"` and resolve to
`Ctx\namespace\foo` — Absent — instead of `Ctx\foo`. Today that is mere
silence (Unknown resolution); under §3/§5 it becomes an
undefined-function/class FP on a defined symbol. Verify how mago
represents the relative identifier; normalize at `name_ref` (strip the
`namespace\` head against the enclosing context) and pin a fixture
before S4.

### M3: monkey-patching extensions void the whole absence family

`uopz`, `runkit7`, `Componere` redefine/augment classes and functions at
runtime — with any of them loaded, *no* absence claim (and no
"final ⇒ immune" §8 leg) holds. One cheap global silence leg: the
sidecar's loaded-extension list is already the §1 oracle; if a
monkey-patch extension is present, the family is Unknown-silent and the
coverage posture says why. (These extensions are dev-tooling; the
posture note will almost never fire in anger, but the leg costs one
lookup.)

### M4: arity §6 must restate the §4 legs it silently depends on

The "uniquely resolved target with ground-truth signature" for a
method call must inherit: the trait leg (a signature found through a
`uses_traits` chain is not ground truth — `resolve_in_chain` already
gives up, `lib.rs:5498`; the §6 emitter must use the same walk, not a
fresh one), the enum leg (G3: empty method lists), and G1's exactness
bit (an alias-of-`$this` receiver is not "proven exact", and §6 itself
proves the declared-receiver variant unsound — the laundering would
reintroduce exactly the override-adds-optionals FP the ADR refuses).
Also inherit §3(d)'s real-call condition (first-class-callable
`f(...)`, currently unrecognized at lowering, must stay out), and note
attributes (`#[Attr(1,2)]`) are not call sites — their arity errors
occur only at `newInstance()`.

### M5: shared key normalization for §7 reads

`lower_array_key` + `php_canonical_int_string`
(`steins-syntax/src/lib.rs:3303-3324`) implement PHP key folding
correctly on the *write* side ("5"→5; "05"/"+5"/"-0"/overflow stay
strings; bool→int; null→""; finite floats truncate). The S3 read-side
lookup must call the **same** helper; a raw-key comparison manufactures
`offset.missing` on `$a = [5 => 'x']; $a["5"]`. One shared function +
one fixture. Negative string offsets (`"abc"[-1]` is valid, no warning)
need their own care if string offsets are in scope.

---

## Part C — Confirmed sound (the guard is sufficient; keep the fixtures anyway)

### C1: the is-a oracle's closure bookkeeping is genuinely complete

`Cx::is_a` (`lib.rs:6524-6566`): the `complete` flag is tainted by any
unresolvable ancestor (`ancestors_of` returns `None` for
not-Unique-and-not-catalogued, and `find_class` returns `None` for
`Ambiguous` — so duplicate FQNs correctly poison closure, tested at
`lib.rs:7505`); cycles terminate via the `seen` set; ASCII
case-folding matches the engine's `zend_str_tolower`. The implicit
`Stringable` hazard (a trait-using visited class might merge a
`__toString`) is explicitly handled — `maybe_stringable` forces Unknown
instead of an unsound No. This is the standard the new ladders should
copy, and evidence the "complete enumeration" sentence is
implementable.

### C2: the trait leg exists per-chain-node in the landed walk

`resolve_in_chain` checks `uses_traits` at **every** node
(`lib.rs:5498`), so "no trait use anywhere in the chain" (§4e) has a
correct landed precedent — including the ancestor-uses-a-trait case.
Traits-using-traits is moot until flattening (any use ⇒ Unknown); when
the deferred flattening lands it must be recursive (trait `use` inside
traits) and must merge trait `__call`/`__callStatic` into the magic
check — write that into the flattening slice now.

### C3: by-ref aliasing cannot fake `Singleton([])`

Reference assignment, by-ref closure capture, `extract`,
variable-variables all set `Scope::poisoned`
(`steins-syntax/src/lib.rs:1351`), and both the refinement path
(`CondOperand::Var` guarded by `!poisoned`, `lib.rs:4148`) and heap
binding (`lib.rs:3296/3310/3317`) are gated on it. The
"aliased write invalidated my proven-empty array" FP class is
structurally blocked — coarse, but the right side of coarse. Spread
elements and non-literal keys collapse the whole array literal to
`Other` at lowering. The §7 opening inherits this for free.

### C4: conditional definitions and duplicate FQNs are correctly *existence*-safe

`lower_classes_into` collects at any nesting depth, so a
`function_exists`/`class_exists`-guarded polyfill is present in the
index and §3/§5 absence can never fire on it; duplicate FQNs demote to
Ambiguous (silence). The gap is identity, not existence — see G2.

### C5: the §6 refusals are anchored in verified semantics

Too-many-to-userland never a finding; the declared-receiver arity
variant refused as *unsound* (override may add optionals) rather than
deferred — both match the evidence rule and need no further guard
(subject to M4's exactness caveat).

### C6: §8's final-member immunity holds (absent M3 extensions)

Extending a final class is fatal even from `eval`; `class_alias`
aliases an existing binding and neither adds methods nor interfaces to
it; there is no runtime construct that makes a final class acquire a
subtype. The `is_final` bit read from a *project* declaration is
trustworthy exactly when G2's identity legs pass — final-ness should be
read from the same arbitration the method list uses.

### C7: `Unknown` handling in the ADR-0052 class arms is FP-safe as specified

Both polarities keep the arm on `Unknown`; negative-branch deletion
requires an inherited `Yes`; positive-branch deletion requires
final/enum **and** closed `No`; emptied lists drop to no-fact rather
than death. Subject to M1 (the quality of Yes/No at builtin edges), the
polarity asymmetry itself is correct — I could not construct a
counterexample that does not route through M1 or G1.

### C8: the §5 non-error positions are correctly excluded

`instanceof`-undefined ⇒ false-as-value-fact, `catch`-undefined ⇒
no-match, `X::class` ⇒ string, type declarations ⇒ call-site
`TypeError` — all kept out of `class.undefined`; trait names join the
class-like set before the id fires (today `Node::Trait` is not lowered
at all, so S1 must add it — when it does, traits must share the *same*
symbol table and ambiguity map as classes, since PHP class-likes share
one namespace).

---

## Sequencing consequences for the slices

1. **Before S2**: G1 (exactness bit + `$this` leg — also fixes two live
   FP paths), G2 legs 1–3, G8(ii) (N2 lands first), G3's written
   silence condition.
2. **Before S3**: G7's context whitelist, M5's shared normalizer.
3. **Before S4**: G5 (include-path demotion), G6 (SAPI posture/allowlist),
   M2 (relative-namespace refs), plus measurement mode as designed.
4. **Before S6/N4**: G4 (anonymous classes), M3's extension guard,
   M1's version gate, and the descendant-scan legs listed in G4(e).
5. The M2-gap-7 (sidecar reflect) slice must carry G3's precondition in
   its own gate, or the enum bomb detonates a milestone later than the
   code that armed it.

Every "verify against PHP 8.5" above (include_path precedence order,
compound-assignment read warnings on missing keys, protected-`__call`
fatality) should go through the ADR-0049 discipline — `php -r`, not
recall — before its leg is worded.
