# A body's sites resolve once: reachable code, implicit user code and obstacles

**Status: proposed (2026-10-02), PENDING ratification.** Designed
autonomously under the owner's standing delegation. Tracking issue #865;
slices #861, #862, #863, #864, #858, #859.

The effect lane (ADR-0005, ADR-0018) and the throw lane (ADR-0040) answer the
same question at every call and operator in a body: *what code can run here?*
They answer it separately. This ADR makes the answer one record, computed once,
that both lanes read.

## 1. Context

### 1.1 The chain that motivated it

Between 2026-09-27 and 2026-10-02, a chain of soundness fixes ran through the
two lanes: #846 (a `new` carries its constructor's effects), #849 (and its
throws), #851 (catalogued pure is certified, ADR-0021's first amendment of
2026-10-01), #852 (the throw subtype walk follows `implements`), and #856
(a builtin call is pure only where its arguments reach no user code,
ADR-0021's second amendment). Each fix was correct, and each exposed the next
hole: #856's amendment itself closes with a list of holes it leaves open
(engine methods and constructors, the throws pass, operators and destructors).

### 1.2 What the code does at c549d8cc

The holes are not missing rows. They come from duplication:

- **Two scans.** `scan_effect_origins` and `scan_throw_origins`
  (`crates/steins-syntax/src/lower_effect.rs`) lower the same tree, each with
  its own anonymous-class twin. They use different receiver vocabularies:
  `$this->repo->m()` is a declared receiver in the effect lane and a bare
  taint in the throw lane. `ThrowKind` carries no argument shapes, so the
  throw lane cannot apply #856's rule even in principle. `eval` and `include`
  have an effect arm and no throw arm: such a body is `{eval, …?}` and
  throw-exhaustive.
- **Seven knowledge predicates.** "The catalog knows this spelling" is
  answered by seven distinct closures handed to function resolution, and some
  forty files read catalog tables directly. The throw lane's "known" is
  `effect_labels` alone, so `array_keys($a)` is effect-exhaustive and
  throw-non-exhaustive.
- **One unsafe default.** The throw lane reads a known builtin with no throw
  row as throwless, at three sites. `strlen($o)` under `@throws void` has an
  empty, exhaustive throw set while `$o->__toString()` throws. Every other
  "no row" default in either lane is already a taint.
- **No model of implicit user code at operators.** `'a' . $o`, `"a{$o}"`,
  `(string) $o`, `$o == 'x'`, `$o->undeclared`, `$o['k']`, `foreach ($o as
  …)` and `clone $o` are each pure and throwless, exhaustively, in both lanes.

### 1.3 What `…?` is made of

On the ten public corpus packages, 22,241 of 28,847 concrete functions and
methods (77%) are `…?`. 87% of those carry a direct cause in their own body;
the rest inherit it. By the sole cause family of a body:

| Sole cause | Bodies |
|---|---|
| dispatch (a variable receiver, a non-final `$this`, a declared receiver, a chain leaving the universe) | 12,326 |
| a state construct (ADR-0055's labels not yet landed) | 1,036 |
| the catalog (an uncatalogued name, an argument's reach, a missing engine row) | 1,031 |
| dynamic code | 43 |

A complete catalog and perfect reach proofs together would release at most
about 5% of `…?`. This ADR is about soundness and structure, not about that
number; §7.2 names where the number moves.

## 2. Decision: one site

1. **Every call-like construct in a body is one site, lowered once.** A site
   (`SiteOrigin`) records its span, its kind, the catch guards around it, its
   operands' shapes (`ArgShape`, ADR-0021's second amendment §4), its
   by-reference targets and its constant arguments. The kinds cover what the
   two scans recorded: named calls and higher-order calls, method calls,
   `new`, callbacks, dynamic callees, `throw`, and the language constructs
   (output, exit, `eval`, inclusion, the state constructs, a `match` with no
   default). §4 adds operator sites. **A lane never scans the tree.**
2. **A site resolves once** into what runs there (edges to declarations, an
   engine function or method, a construct, or nothing), what the engine may
   run implicitly on its operands (reach), and the obstacles that make the
   answer incomplete. **A lane reads only its own axis's rows, and only
   through the resolver**; it never resolves a name and never sees a missing
   row.
3. The site list is part of the trace payload, which is decoded past the
   analyzer gate (ADR-0092's amendment of 2026-09-27), so introducing it needs
   no `SCHEMA_VERSION` bump. The wire codec reads variants by index, so the
   order of `SiteKind` is a format fact and is pinned by a persistence test.

## 3. Decision: one knowledge, one default

1. **A spelling is a builtin if and only if the catalog knows it**
   (`steins_catalog::knows`): one predicate, read by both lanes and by
   function resolution. The catalog keeps its per-axis tables; what becomes
   single is the question asked of them.
2. **A known name with no row on an axis is an obstacle on that axis**,
   never pure and never throwless. A plugin's declaration is consulted first
   (ADR-0068's precedence).
3. **A throw row may be omitted only where throwlessness is evidenced**: the
   fold allowlist (audited in #320) and the certified-pure list (ADR-0021's
   first amendment of 2026-10-01). The call-site certified list is not
   throw-evidenced (`unpack` raises `ValueError` on an unknown format code)
   and enters only name by name, with a row or a witness.

## 4. Decision: reach at every site

1. **ADR-0021's call-site rule (second amendment of 2026-10-01, §3) holds at
   every site kind, in both lanes**: builtin functions, engine methods and
   constructors, and operator sites, from the same operand shapes. Where an
   operand may reach user code, the site keeps its row and adds an obstacle;
   in the throw lane that obstacle makes the throw set non-exhaustive,
   because unknown user code may throw anything.
2. **Engine methods and constructors** get a per-method reach row
   (`method_arg_reach`), curated and witnessed for each method the catalog
   already rows on another axis. A method with no reach row reaches blind.
3. **Operator sites.** An operator site carries its operands' shapes. A class
   is *exact* when named by `new Foo`, `Foo::` or `parent::`, or when a
   receiver is declared as a final in-universe class; it is *bound* for
   `$this`, `self`, or a receiver declared as a non-final class or an
   interface. A union is ruled out only when every member is; `iterable`
   reads as `array|Traversable`.

   | Family | Constructs | Nothing runs when | Edge when | Otherwise |
   |---|---|---|---|---|
   | ToString | `.`, interpolation, `(string)`, `echo`/`print`, `==`/`!=` against a string | the operand is object-free, or an exact class's closed chain has no `__toString` | an exact class declares `__toString`, or a bound class's is final | obstacle |
   | MagicProp | a property read, write, `isset`, `unset` | the operand is not an object, or the name is a declared non-private property (or a private one of the receiver's own class), or an exact class's chain has no magic method | an exact class declares the magic method, or a bound class's is final | obstacle (a hooked property too) |
   | ArrayAccess | `$x[k]` read, write, `isset`, `unset` | the operand is not an object, or an exact class is proven not `ArrayAccess` | an exact class is `ArrayAccess` (an engine class reads its rows), or a bound class's `offset*` is final | obstacle |
   | Iterate | `foreach`, `yield from`, spreading a non-array | the operand is not an object, or an exact class is not `Traversable` | an exact `IteratorAggregate` or `Iterator` whose methods resolve, or a bound class's are final | obstacle |
   | Clone | `clone` | an exact class's chain has no `__clone` | an exact class declares `__clone`, or a bound class's is final | obstacle |
   | Call | a method absent from a complete chain on a class declaring `__call`/`__callStatic` | — | an exact class, or a bound class's magic method is final | obstacle |

   A bound class is an obstacle where an exact one runs nothing because a
   subclass may add the magic method (`Stringable` is implicit) or implement
   the interface. `__invoke` is a dynamic callee and stays one. Files
   declaring each magic method on the public corpora (6,482 files):
   `__invoke` 59, `__toString` 56, `getIterator` 37, `__destruct` 20,
   `__clone` 12, `__call` 9, `__get` 8, `__isset` 6, `__callStatic` 5,
   `__set` 3, `offsetGet` 3, `__unset` 0.

## 5. Decision: obstacles replace the bit

1. **Non-exhaustiveness is a set of recorded reasons.** An own row carries
   the obstacle kinds its sites produced instead of a bare `exhaustive` flag;
   `…?` is the set's non-emptiness. The kinds name a dynamic callee, an
   unknown class, an open method, unseen code, an unresolved callback, user
   code an operand may reach, a missing engine row (by axis), a state
   construct, and an interop envelope.
2. **No obstacle is discharged by any policy in this ADR.** ADR-0084 §3
   keeps tolerances away from spelling-producing sites, and no consumer of
   exhaustiveness (the annotate margin, `effect-diff`, `effects-envelope`,
   `loop-to-array-map`) has a floor; `check` reads labels only.
3. The kinds surface where exhaustiveness already does: `annotate --format
   json` and the effect-baseline entry, so that every coverage change in an
   A/B names its cause.
4. Own rows live in the facts payload, past the analyzer gate: no
   `SCHEMA_VERSION` bump.

## 6. What moves

No slice adds a proven label, so `check --profile strict` and every
proven-lane finding are unchanged throughout; exhaustiveness never
manufactures a finding. What moves is the `…?` marker, the effect baseline,
the tags `effects-envelope` writes and the loops `loop-to-array-map` accepts.

| Slice | Issue | Expected on the public corpora |
|---|---|---|
| fixpoints once per run | #861 | byte-identical |
| one site scan | #862 | byte-identical |
| one resolver, obstacles | #863 | byte-identical |
| one knowledge, one default | #864 | throw lane: about 150 bodies become exhaustive (`array_keys`, `dirname`), about 206 become `…?` (reach, `eval`/`include`) |
| engine methods and constructors | #858 | small: most Throwable constructor arguments are literals or `sprintf` values |
| operator sites | #859 | at most 340 of 6,606 exhaustive bodies become `…?` (concatenation 134, non-`$this` property access 90, `foreach` 75, interpolation 47, cast 17, loose equality 17, `echo` 7, `unset` 4, `clone` 3), before operand proofs |

Each slice's pull request records its measured diff, classified. The corpus
checkouts carry no `vendor/`: 1,310 dispatch-sole bodies are universe
incompleteness alone, so the absolute numbers understate what a full project
sees.

## 7. Deliberately open

### 7.1 Destructors

Every reassignment, `unset` and scope exit may run a `__destruct`. A
per-site obstacle would make exhaustiveness vanish, and §5.2 leaves no floor
to calibrate it against; recording an obstacle no derivation reads would be
dead data. So this ADR records the hole and **no obstacle for it**. Two
follow-up designs, either of which is a new decision: edges to an exact
class's `__destruct` only (sound for the exact case, cheap, since 20 corpus
files declare one); or a universe gate (some class declares `__destruct`)
recording an obstacle that a strict floor reads, which first needs a floor
for exhaustiveness.

### 7.2 Dispatch

The lever on `…?` is dispatch (§1.3). Resolving a site whose receiver the
walk has pinned (`$r = new Repo(); $r->find()` is `…?` while the margin
prints `$r: Repo (exact)`), a declared receiver typed to a final in-universe
class (143 sole bodies), and, under a closed-universe posture, a non-final
`$this` with no subclass anywhere (805) or an interface receiver (248) would
fill the resolver's edges from the walk. That needs own rows refined by the
walk and a decision on universe closure against ADR-0043 §3 and ADR-0015, so
it is its own ADR. The resolver's edge target is the seam it lands in.

### 7.3 Phase order

The effects fixpoint runs before the walk whose narrowed facts could rule
more operands out. The walk reads only proven findings from it, never
exhaustiveness, so this is a limit on precision, not a cycle; #856 measured
6 bodies bought by a call's declared return type. It is revisited with §7.2.

## 8. Relation to earlier decisions

- **ADR-0021** (second amendment of 2026-10-01): its call-site rule now holds
  at every site kind (§4); `knows` replaces the per-pass knowledge closures
  (§3).
- **ADR-0040** §2: the four sources of throw facts are read off the site
  record; a known name with no throw row is an obstacle, never throwless
  (§3.2).
- **ADR-0046** (amendment of 2026-09-26): `eval` and an inclusion make both
  lanes non-exhaustive.
- **ADR-0067**: declared receivers resolve the same way in both lanes.
- **ADR-0055** (amendment of 2026-09-26): state constructs stay obstacles
  until their labels land; operator sites follow §4.
- **ADR-0084** §3: unchanged; it is why §7.1 waits.
