# Trait members are composed at query time, and trait bodies are walked with an open receiver

**Status: proposed (2026-10-14), PENDING ratification.** Designed by the
architect for the type/arity reproduction run (issue #1051, decision D1, which
the owner adopted on 2026-10-11). Lands with slice S8 (#940, #952 l3). Reopens
ADR-0049 §4 leg (e), §5's trait note and A18's trait leg, and ADR-0078's trait
silence leg; the ADR-0049 amendment of the same date points here.

## 1. Context

A trait's body was lowered by name only (`lower_trait` emitted no methods,
properties or constants), and `collect_scopes` built method scopes under a class
alone, so a trait's, an enum's and an anonymous class's method bodies were never
walked as bodies. Every consumer that reads a class's members then treated
`ClassDecl::uses_traits` as a blanket obstacle: the arity chain, the member
absence and inaccessibility families, the declaration fatals, the dispatch
resolver, the declared-receiver descendant scan, `method_exists`, the class
constant reach, and the promote transform. One `use T;` of an empty trait made a
class unenumerable. Four fatals PHP 8.5.11 raises were silent (#940 a–d), and so
was a typed property a trait declares (#952 l3):

| witness | PHP 8.5.11 |
| --- | --- |
| a) `trait T { abstract public function p(): int; } class U { use T; }` | Class U contains 1 abstract method … (U::p) |
| b) a trait method over a `final` parent method | Cannot override final method Base::foo() |
| c) `new C()` on `class C { use T; __construct(int $a) }` | Too few arguments to function C::__construct() |
| d) `missing()` inside a trait method | Call to undefined function missing() |
| l3) `$q->t = "x"` for a trait's `public int $t` (strict) | Cannot assign string to property Q::$t of type int |

## 2. Decision

### 2.1 D1: trait members are lowered, and composed at query time

A trait's methods, properties and constants lower exactly as a class's do
(`lower_body_members`, shared by class, trait and enum), and an enum's methods
lower too. Nothing is copied into the importing class. What a class or enum
*holds* is asked through one per-universe memo, `TraitMemo`, which `Index`
carries (`crates/steins-infer/src/trait_composition.rs`):

- `Cx::held_method`, `held_methods`, `held_property`, `held_properties`,
  `held_const`: the class-like's own member first, then the one its composition
  imports; `Err(Open)` where the composition declines.
- `Cx::trait_hides`: the narrow obstacle for a consumer that reads a node's own
  lists for a member name (the composition declines a table, or imports a member
  of that name).
- `Cx::reached_traits`: the trait declarations a composition reaches (the A14
  magic-tag reach and the hooked-property check read them).

The memo is **lazy per class-like**: a composition is computed when a consumer
first asks about its class, so a warm run decodes only the trees its queries
reach (issue #516); a universe-wide build would decode every tree. A trait's
whole member set (own and imported) is computed once and shared by every
importer, in post-order over the `use` graph **without recursion**, so a `use`
graph thousands deep costs neither stack nor repeated work. A declaration in an
unparsable file is member-incomplete (ADR-0079 §2.5), and since the dam is a
per-run answer and the memo is not, that leg is asked at query time.

Inheritance stays the consumer's: a chain walk reads what each class holds, then
goes to its parent, which is PHP's order (class member over trait member over
inherited member).

### 2.2 The composition rule table

Each row is witnessed on PHP 8.5.11; "decline" is an `Open` answer, so the
consumer treats the class-like as unenumerable, as every trait user was before.
The table is the doc comment of `trait_composition.rs`, and
`crates/steins-infer/tests/it/trait_composition.rs` pins one test per row.

| row | shape | PHP | here |
| --- | --- | --- | --- |
| C1 | the importer declares a member a trait declares | the importer's wins | own member first |
| C2 | a trait method and an inherited one | the trait's wins | held members before the parent's |
| C3 | two traits, one concrete name, different declarations | fatal collision | decline the method table |
| C3a | one abstract, one concrete | the concrete runs | the concrete is held |
| C3b | both abstract | loads (abstract importer) | the first is held |
| C4 | one declaration reached twice (`use T, T`, a diamond) | one member | one member |
| C5 | `insteadof` / `as` anywhere in the composition | renames, excludes, re-scopes | decline every table (v1) |
| C6 | a trait using traits | own members shadow imported ones | recursive; a cycle declines |
| C7 | a used name that is no unique, unconditional trait | not found, or load order decides | decline every table |
| C8 | an abstract trait method nothing implements | fatal naming `Importer::m` | the importer's requirement |
| C8a | … that an inherited method implements | loads (an inherited abstract one is named) | discharged by the chain |
| C9 | a concrete trait method (private too) over a `final` parent method | Cannot override final method `Base::m()` | the importer's override, reported at its `use` |
| C9a | an abstract trait method over a `final` parent method | loads | never an override |
| C10 | static and instance members | composed alike | no distinction |
| C11 | a property two sources declare (importer and trait, or two traits) | compatible loads, else fatal | importer's own first (C1); two traits decline the property table |
| C11a | a trait's hooked property | a hook is user code | decline the property table |
| C12 | a constant two sources declare (PHP 8.2) | as C11 | as C11, for the constant table |

C1 holds even where the composition declines: a class's own member outranks
anything a `use` imports or an adaptation names, so `held_method` answers an own
member before it consults the composition at all.

### 2.3 Trait, enum and anonymous-class bodies get scopes

- A **trait** method is a `ScopeOwner::TraitMethod { class, method }`, `class`
  being the trait's FQN. Its body runs in whichever class imports it, so the one
  walk judges it with the receiver **open**: `scope_class` answers `None` (as for
  a closure), `class_scope_known` answers `false` (no visibility claim from a
  scope the walk does not know), the effect and throw units carry no class, and
  a trait's `self`/`static`/`parent` return keyword is never stamped with the
  trait's name. Receiver-independent families judge the body (`call.undefined-
  function`, argument types against builtins, `return.compile-fatal`, the
  `untyped.*` and docblock lints); every receiver-dependent one declines. A
  trait's property hooks get no scope (a hook scope names its class).
- An **enum** method is a `ScopeOwner::Method` of the enum, whose `$this` is the
  enum itself (enums are final).
- An **anonymous class** method is a `ScopeOwner::AnonMethod { def_offset, method }`:
  no `ClassDecl` carries its members, so its parameters and native return type
  ride on the scope, as a property hook's do, and its receiver is open.

Judging a trait body per importer (binding `$this` where one importer is the
only one) is a refinement this ADR leaves for later; the open receiver is the
sound floor it would refine.

### 2.4 What reads the composition, and what keeps the obstacle

Routed: `walk_arity_chain`; `method_exists_verdict`; the class-constant reach
(`providers`, `enumerate_const_reach`, the constant-initializer reach); the
method and property absence walks and their descendant scans; the A14 magic-tag
reach (a tag on a trait obstructs its importers); `enumerate_member_chain`
(inaccessibility, through `trait_hides`); the declaration fatals (requirements,
satisfaction, the overriding family over the subject's own and imported
concrete methods); `resolve_in_chain_mode`, `builtin_root` and `engine_exit`;
the private-shadow rule; `Cx::class_props`, `class_body_hooked`, `find_ctor`;
`write_misses_declared_slot`; implicit `Stringable`; `nearest_parent_throws` and
`nearest_parent_effect`; the call-site reach's `__get` and parameter lookups;
the affected set, which now follows the lowered `use` names instead of every
class the file names.

Kept, with the reason:

- **Dispatch descends into no trait method.** `resolve_in_chain` under
  `ChainMode::Dispatch` answers `Unknown` for an imported method: the body is
  walked once with an open receiver, never per call site (§2.3). Under
  `ChainMode::Declaration` (ADR-0049 A16/A18) it answers the trait's
  declaration, whose envelope binds by return covariance as an inherited one's
  does; a declining composition still walks on to the parent there, as before.
- **The operator chain** (`site/operator/chain.rs`) keeps `closed &= !uses_traits`:
  it reads hint text and visibility in the declaring class-like's scope, which for
  a trait member is its importer's.
- **`class_const_flags`** (a phpdoc `C::FLAG_*` pattern) keeps the obstacle: it
  enumerates constants, which the composition answers only by name.
- **The promote transform's eligibility** keeps `uses_traits` and adds `is_trait`
  ("a trait method is copied into every class that uses it"); the four edit
  transforms keep trait methods out of their candidates, and the
  effects-envelope class tag still refuses a trait user.
- **Early binding** (ADR-0049 A26) keeps `uses_traits`: it is PHP's own rule.
- **A private method imported from a trait** answers nothing for the
  private-shadow rule and is `private_blocked` from its importer, a decline.

### 2.5 Messages name the importer

PHP names an imported method by its importer (`C::__construct()`, `U::p`,
`Child::foo()`), and so do the arity, abstract-unimplemented and overriding
messages. A finding inside a trait's body (`type.return-mismatch`,
`type.return-missing`) names the one class that imports the trait directly, when
there is exactly one, and the trait otherwise: the one walk speaks for every
importer. An overriding finding for an imported method is reported at the
importer's `use` name it came through, the line in the importer's file the fix
starts from.

### 2.6 A call condition the fold decides is decided

Resolving a trait user's own methods (C1) let the binding descent (ADR-0001,
ADR-0057) reach `CarbonInterval::createFromFormat("H:i:s", null)` and report a
proven `TypeError` at an `explode($m[1], $interval)` under
`if (preg_match('/s([,.])([uv])$/', $format, $m))`. PHP decides that condition
false for this call and the program runs clean; the walk left it undecided,
because a call in truthiness position (`if (f(…))`) was never folded, though the
same call in a comparison (`if (f(…) === 1)`) was. The shape predates this ADR:
the plain walk and a descent into a class with no trait report it alike.

The rule is the comparison's, applied to truthiness: a call condition the fold
resolves over the walk's proven arguments is decided by the value's truthiness,
in the plain walk and in a descent alike. A trailing `$matches` of `preg_match`
or `preg_match_all` is written and never read, and the count the call returns
does not depend on it, so the call folds without it. Nothing else changes: an
undecided condition leaves both sides judged ("when the line runs"), and a
decided one prunes its dead side as every decided condition does. The rows, each
on PHP 8.5.11 for `outer("abc", "abc")`, are pinned in `tests/it/binding.rs`:

| row | condition | sides holding `width($s)` | PHP | here |
| --- | --- | --- | --- | --- |
| b01 | `preg_match('/x/', $f)` (false) | then, else | TypeError | reported, on the live else side |
| b02 | `preg_match('/a/', $f)` (true) | then | TypeError | reported |
| b03 | `preg_match('/q/', $f) \|\| rand() >= 0` | then | TypeError | reported (`rand()` leaves it open) |
| b06 | `preg_match('/a/', $f)`, then `rand() >= 0` inside | inner then | TypeError | reported |
| b07 | `$g = $f . ""; preg_match('/q/', $g)` | then | clean | silent (`$g` is proven `"abc"`) |
| b09 | `preg_match('/q/', $f, $m)` (false) | then | clean | silent |

Without a live sidecar nothing folds, and the condition stays undecided, as
before.

**A contradicted assertion declines the rest of a descent's path.** The same
exposure reached a descent past an assertion helper: `null` bound into a
parameter a callee asserts `string` (`@phpstan-assert`, or an
`-if-true`/`-if-false` assert taken as a guard). The helper throws there, or its
docblock lies and it returns; neither is safe to prove on, so the descent
declines: once a call's assert is contradicted by every value the walk proves for
the argument, the binding descent reports nothing further on that path
(`Store::withheld`, kept through a join only where every joined path carries it).
The path is not marked dead: its env flows on, and every other analysis reads it
as before. This only removes findings, so no proof rests on the docblock claim.
It holds for every way the descent reaches the call (a plain call, a static call,
a method call on a resolved receiver, an assert nested in an argument), pinned in
`tests/it/binding.rs`.

## 3. Persisted formats

No layout read before the analyzer gate changes, so `SCHEMA_VERSION` does not
move (generation-schema.md, the 2026-09-27 narrowing). The `symbols` shard's
*contents* grow (the trait and enum docblocks feed its magic-obstacle records, a
trait's lowered methods reach `magic_property_classes`, an enum's `use` reaches
`trait_users`), but its `PackageShard` fields are unchanged, and a shard reaches
no finding across analyzer versions. The `ClassDecl` and `Scope` changes
(`trait_adapted`, the lowered trait and enum members, `ScopeOwner::TraitMethod`
and `AnonMethod`) are in the trace payload, which the analyzer fingerprint gates.

## 4. Consequences

Measured in the PR (public corpus, base `d886f539`): see the PR body for the
A/B and its classification. Every moved finding is either a trait user's own
member now resolved (C1), a member a trait gives it (C2), or a trait, enum or
anonymous-class body now walked with an open receiver.

The effect lane moves with the call resolution. A static call on a class that
uses traits used to be an `unknown-class` gap; it now reaches its body, so a
wrapper such as Chronos's `createFromFormat` (a class composing traits, over
`DateTimeImmutable::createFromFormat`) carries the engine row's zone and clock
labels (ADR-0101 §3.17) to every caller, definite exactly where the same wrapper
on a class with no trait already was. Nothing in this is reached through an
unbounded receiver set: a string conversion of a `mixed` operand stays the
possibly-grade gap.

Not built here, each a follow-up: `insteadof`/`as` (C5) composed rather than
declined; judging a trait body per importer; the anonymous class at its
`new class(...)` site (D2, #941 c/d); the operator chain reading the composition.
