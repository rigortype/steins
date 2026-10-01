# A body's sites resolve once: reachable code, implicit user code and coverage gaps

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
two lanes: issue #804 (a `new` carries its constructor's effects), #849 (and
its throws), #851 (catalogued pure is certified, ADR-0021's first amendment of
2026-10-01), #852 (the throw subtype walk follows `implements`), and #856 (a
builtin call is pure only where its arguments reach no user code, ADR-0021's
second amendment). Each fix was correct, and each exposed the next hole:
#856's amendment itself closes with a list of holes it leaves open (engine
methods and constructors, the throws pass, operators and destructors).

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
  answered by seven distinct closures handed to function resolution, and over
  forty source files read catalog tables directly. The throw lane's "known"
  is `effect_labels` (and `invocation_shape` for an invoker), so
  `array_keys($a)` is effect-exhaustive and throw-non-exhaustive.
- **An unsafe default.** The throw lane reads a known builtin with no throw
  row as throwless, at four sites (the call arm, the higher-order fallback,
  the invoker arm, and the callback helper). `strlen($o)` under
  `@throws void` has an empty, exhaustive throw set while `$o->__toString()`
  throws. `statement.no-effect` reads the same default.
- **No model of implicit user code at operators.** `'a' . $o`, `"a{$o}"`,
  `(string) $o`, `$o == 'x'`, `$o->undeclared`, `$o['k']`,
  `foreach ($o as …)` and `clone $o` are each pure and throwless,
  exhaustively, in both lanes.

### 1.3 What `…?` is made of

Measured at c549d8cc with a throwaway probe that attributes each body's
direct causes, joined to `effect-diff --set-baseline` per package (the probe
agrees with the analyzer on all but 11 of 28,847 bodies). On the ten public
corpus packages, 22,241 of 28,847 concrete functions and methods (77%) are
`…?`. 87% of those carry a direct cause in their own body; the rest inherit
it. By the sole direct cause family of a body:

| Sole cause | Bodies |
|---|---|
| dispatch (a variable receiver, a non-final `$this`, a declared receiver, a chain leaving the universe) | 12,326 |
| a state construct (ADR-0055's labels not yet landed) | 1,036 |
| the catalog (an uncatalogued name, an argument's reach, a missing engine row) | 1,031 |
| dynamic code | 43 |

A complete catalog and perfect reach proofs together would directly release
about 5% of `…?`. This ADR is about soundness and structure, not about that
number; §7.2 names where the number moves. Every other figure in this ADR
comes from the same probe and is an estimate until the slice that moves it
measures it.

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
   run implicitly on its operands (reach), and the coverage gaps that make the
   answer incomplete. **A lane reads only its own axis's rows, and only
   through the resolver**; it never resolves a name and never sees a missing
   row.
3. The site list is part of the trace payload, which is decoded past the
   analyzer gate (ADR-0092's amendment on what the schema number covers), so
   introducing it needs no `SCHEMA_VERSION` bump. The wire codec reads
   variants by index; a persistence test round-trips every `SiteKind`.

## 3. Decision: one knowledge, one default

1. **A spelling is a builtin if and only if the catalog knows it**
   (`steins_catalog::knows`): one predicate, the union of the tables the
   seven closures read (effect labels, out-parameters, by-value
   certification including the mined parameter facts it reads, invocation
   shapes, the pure-at-arity and call-site certified lists), read by both
   lanes and by function resolution. Two
   questions stay outside it: the shadow check (a user declaration shadowing
   a builtin), which keeps answering no, and runtime-existence questions
   (`function_exists` folding, the absence family), which keep their own
   tables.
2. **A known name with no row on an axis is a coverage gap on that axis**,
   never pure and never throwless. A row on the effect axis is any of effect
   labels, out-parameters, or a certification; a row on the throw axis is a
   `builtin_throws` row or §3.3's evidence. A plugin's declaration enters the
   declared lane (ADR-0068) and the gap stays.
3. **A throw row may be omitted only where php-src shows the name raises
   nothing for any argument its parameter types admit**, argument checking's
   `TypeError` and `ArgumentCountError` aside, which the throw lane does not
   model. Argument *binding* is argument checking too: an unknown named
   parameter (`Error`) and a spread or arity mismatch (`ArgumentCountError`)
   are raised before the callee runs, whichever builtin it is, and
   `array_key_exists` and `key_exists` raise a `TypeError` for an array or
   object key because their stub types the key `mixed`. None is a throw row. A
   throw that depends on *which value* an admitted argument holds
   (`array_column([['a' => [1], 'b' => 1]], 'b', 'a')` is a `TypeError`) is not
   argument checking and gets a row. A row that omits a flag-dependent throw
   (`JSON_THROW_ON_ERROR`) is a gap unless the flags argument is a constant
   expression the scan evaluates and it lacks the flag. The fold
   allowlist and the certified lists are *candidates* for that evidence, not
   evidence: `json_encode` throws `JsonException` under its flag,
   `preg_match_all` raises `ValueError` on bad flags. Each name is audited
   before it is read as throwless (#864); the audit's list is a catalog table
   with a witness per name.

## 4. Decision: reach at every site

1. **ADR-0021's call-site rule (second amendment of 2026-10-01, §3) holds at
   every site kind, in both lanes**: builtin functions, engine methods and
   constructors, and operator sites, from the same operand shapes. Where an
   operand may reach user code, the site keeps its row and adds a gap; in the
   throw lane that gap makes the throw set non-exhaustive, because unknown
   user code may throw anything.
2. **Engine methods and constructors** get a per-method reach row
   (`method_arg_reach`), curated and witnessed for each method the catalog
   already rows on another axis. A method with no reach row reaches blind.
3. **Operator sites.** An operator site carries its operands' shapes. The
   rules below speak of user code only; the engine's own `Error`-family
   raises at operators are §7.4's.

   A class is *exact* when the operand is `new Foo`, a receiver declared as a
   final in-universe class, or `$this` in a final class. It is *bound* for
   `$this` in a non-final class, for a static form (`self::`, `parent::`,
   `static::`, `Foo::`) in object context, and for a receiver declared as a
   non-final class or an interface. A method is final when it is declared
   final or declared in a final class. An engine class
   answers through its catalog rows, and a missing row is a gap (§3.2). A
   union is ruled out only when every member is; `iterable` reads as
   `array|Traversable`. A *closed chain* is one whose every ancestor, trait
   and interface is in the universe.

   | Family | Constructs | Nothing runs when | Edge when | Otherwise |
   |---|---|---|---|---|
   | ToString | `.`, `.=`, interpolation and heredocs, `(string)`, `echo`/`print`, a loose or ordering comparison (`==`, `!=`, `<`, `<=`, `>`, `>=`, `<=>`, `switch`) with any operand not shown object-free (two objects compare their properties, which may convert) | the operand is object-free, or an exact class's closed chain has no `__toString` | an exact class declares `__toString`, or a bound class's is final | gap |
   | MagicProp | a property read, write, `isset`, `empty`, `unset`, `??`, `??=`, a reference to it | the operand is not an object; or the class is exact, its closed chain declares no `__get`/`__set`/`__isset`/`__unset` and hooks no property of that name | an exact class declares the magic method or the hook | gap (§4.4 for a bound class's declared property) |
   | ArrayAccess | `$x[k]` read, write, `isset`, `empty`, `unset`, `??`, `??=`, a reference to it, list destructuring | the operand is not an object, or an exact class is proven not `ArrayAccess` | an exact class is `ArrayAccess`, or a bound class's `offset*` methods are final | gap |
   | Iterate | `foreach`, `yield from`, spreading a non-array | the operand is not an object, or an exact class's closed chain is not `Traversable` and hooks no property | an exact `Iterator`'s methods, or an exact `IteratorAggregate`'s `getIterator` together with the methods of the iterator it is shown to return; a bound class's when final | gap |
   | Clone | `clone`, and `clone` with a property list | an exact class's closed chain has no `__clone`, no `__set` where a property list is given, and hooks no property | an exact class declares `__clone` (or `__set`), or a bound class's is final | gap |
   | Call | a method absent from, or inaccessible in, a complete chain on a class declaring `__call`/`__callStatic` | — | an exact class, or a bound class's magic method is final | gap |

   A bound class is a gap where an exact one runs nothing because a subclass
   may add the magic method (`Stringable` is implicit), implement the
   interface, or hook an inherited property. `__invoke` is a dynamic callee
   and stays one. Files declaring each magic method on the public corpora
   (6,482 files): `__invoke` 59, `__toString` 56, `getIterator` 37,
   `__destruct` 20, `__clone` 12, `__call` 9, `__get` 8, `__isset` 6,
   `__callStatic` 5, `__set` 3, `offsetGet` 3, `__unset` 0.
4. **A bound class's declared property.** `$this->p` on a declared property
   visible in the accessing scope runs user code only if the property is
   unset and a class in the object's runtime chain declares the matching
   magic method, or a class hooks it. Read as a gap unconditionally, every
   such read would join the dispatch question of §7.2. The default is
   therefore a universe gate: the access is a gap when the bound class's
   closed chain or some in-universe subclass declares one of the four magic
   methods or hooks a property of that name, and when the chain is not
   closed; it runs nothing otherwise. A subclass outside the universe is
   §7.2's open question, named there.
5. **Lazy objects, error handlers, tick and signal handlers** are attributed
   to their registration, as ADR-0021 §3 already does for error handlers in
   the effect lane: a lazy object's initializer is the callback argument of
   `ReflectionClass::newLazyGhost`, `newLazyProxy` and the `resetAsLazy*`
   pair, and is reached there (a `Callback` reach, §4.2); likewise
   `set_error_handler`, `register_tick_function` and `pcntl_signal`; and
   user stream wrappers and filters: a class named to `stream_wrapper_register`
   or `stream_filter_register`, or a registered filter attached by
   `stream_filter_append` and `stream_filter_prepend`, is reached at that call
   (an `Autoload` reach in the catalog's table, which no call site rules out).
   The wrapper's methods and the filter's `filter()` then run inside later
   `file_exists`, `is_dir`, `filesize`, `fwrite`, `fseek`, `fclose` and their
   kin, which therefore stay on the throwless table: the registering body
   carries the gap, and the I/O call does not repeat it. The throw lane
   follows the same attribution, so an error handler that throws
   `ErrorException` is a gap at its registration. Autoloaders stay as they are: the builtin rows that
   autoload carry ADR-0021's `Autoload` reach, and a class-naming construct
   (`new Foo`, `Foo::`) is not charged for one; unifying the two is
   follow-up.
6. **Coercion at user boundaries** is a ToString site too. An object handed
   to a user function's or method's parameter, returned from a function, or
   assigned to a typed property runs `__toString` when the declared type
   admits `string` but not the object's class (`?string`, `string|int`,
   `string|array`) and the governing file is coercive. The governing file
   differs by boundary: a parameter follows the **calling** file's
   `strict_types`, a return the **declaring** file's, a property write the
   **writing** file's. The rule is the ToString row's, applied to the
   declared type. Its implementation is issue #868, after #859 (§7.5).

## 5. Decision: coverage gaps replace the bit

1. **Non-exhaustiveness is a set of recorded reasons.** An own row carries
   the gap kinds its sites produced instead of a bare `exhaustive` flag;
   `…?` is the set's non-emptiness, joined over the edges the body reaches.
   Exhaustive means exhaustive modulo the handlers §4.5 attributes to their
   registration and the destructors §7.1 leaves open: an `ErrorException`
   from an error handler, or an exception from a lazy initializer, escapes
   from the triggering body at runtime while that body's throw set reads
   exhaustive. `loop-to-array-map` (ADR-0076 §2.3) inherits the same
   qualification.
   Every place that clears the flag today maps to one kind: a dynamic callee,
   an unknown or ambiguous function, an unknown class, an open method, a
   non-final `$this`, a declared receiver, an unresolved callback, unseen
   code, an operand that may reach user code, a missing row (by axis), a
   state construct, an interop envelope, an unresolvable thrown or rethrown
   class, and an argument list (named or spread) that defeats arity.
2. **No gap is discharged by any new policy.** The existing discharges keep
   their reach: an untainting edge (ADR-0063) and a checked envelope at a call
   (ADR-0067) discharge gaps exactly as they discharge the flag today, and
   narrowing them to the kinds their contracts answer is follow-up. A gap
   under a provably absorbing `catch (\Throwable)` is not dammed yet (a later
   precision gain). No consumer of exhaustiveness (the annotate margin,
   `effect-diff`, `effects-envelope`, `loop-to-array-map`) has a floor, so a
   gap cannot be calibrated per profile; this is why §7.1 waits.
3. **The kinds surface where exhaustiveness already does** — `annotate
   --format json` and the effect-baseline entry — so that every coverage
   change in an A/B names its cause. That surfacing is a user-visible change
   (the baseline format moves) and lands with #864, not with the refactor.
4. Own rows live in the facts payload, past the analyzer gate: no
   `SCHEMA_VERSION` bump.

## 6. What moves

Slices #861, #862, #863 and #858 add no proven label. #859 adds edges to
magic methods whose callees' labels and throws propagate, and #864 changes
which global user functions shadowing a newly known name resolve; both
record their `check --profile strict` diff. Elsewhere what moves is the `…?`
marker, the effect baseline, the tags `effects-envelope` writes, the loops
`loop-to-array-map` accepts, and `statement.no-effect` where it read the
throwless default.

| Slice | Issue | Expected on the public corpora |
|---|---|---|
| fixpoints once per run | #861 | byte-identical |
| one site scan | #862 | byte-identical |
| one resolver, coverage gaps | #863 | byte-identical (no surface shows the kinds yet) |
| one knowledge, one default | #864 | throw lane: some bodies become exhaustive (`array_keys` once audited), about 206 become `…?` directly (reach, `eval`/`include`), plus the audit's unevidenced names; the baseline format moves |
| engine methods and constructors | #858 | not yet measured; a `sprintf` value is an unproven shape, so `new \RuntimeException(sprintf(…))` in a coercive file gains a gap |
| operator sites | #859 | about 340 of 6,606 exhaustive bodies become `…?` directly, before operand proofs (concatenation 134, non-`$this` property access 90, `foreach` 75, interpolation 47, cast 17, loose equality 17, `echo` 7, `unset` 4, `clone` 3); callers inheriting them come on top |

Each slice's pull request records its measured diff, classified. The corpus
checkouts carry no `vendor/`: 1,310 dispatch-sole bodies are universe
incompleteness alone, so the absolute numbers understate what a full project
sees.

## 7. Deliberately open

### 7.1 Destructors

Every reassignment, `unset` and scope exit may run a `__destruct`. A per-site
gap would make exhaustiveness vanish, and §5.2 leaves no floor to calibrate
it against. So this ADR records the hole and adds no gap for it; #859 does
not close its destructor part. Two follow-up designs, either of which is a
new decision: edges to an exact class's `__destruct` only (sound for the
exact case, cheap, since 20 corpus files declare one); or a universe gate
(some class declares `__destruct`) whose gap a strict floor reads, which
first needs a floor for exhaustiveness.

### 7.2 Dispatch and the universe

The lever on `…?` is dispatch (§1.3). Resolving a site whose receiver the
walk has pinned (`$r = new Repo(); $r->find()` is `…?` while the margin
prints `$r: Repo (exact)`), a declared receiver typed to a final in-universe
class (143 sole bodies), and, under a closed-universe posture, a non-final
`$this` with no subclass anywhere (805) or an interface receiver (248) would
fill the resolver's edges from the walk. The same posture answers §4.4's
subclasses outside the universe. That needs own rows refined by the walk and
a decision on universe closure against ADR-0043 §3 and ADR-0015, so it is
its own ADR. The resolver's edge target is the seam it lands in.

### 7.3 Phase order

The effects fixpoint runs before the walk whose narrowed facts could rule
more operands out. The walk reads only proven findings from it, never
exhaustiveness, so this is a limit on precision, not a cycle; #856 measured
6 bodies bought by a call's declared return type. It is revisited with §7.2.

### 7.4 The engine's own raises at operators

`'a' . $noToString`, `(string) $enum`, `$plainObject['k']`, `$null->p = 1`,
`$a % 0`, `1 << -1`, `[] + 1` and `clone $enum` raise `Error`-family
exceptions without running user code. §4's table is about user code; these
raises are out of the throw lane's model today, as argument checking's
`TypeError` is (§3.3), and modelling them is a separate decision.

### 7.5 Coercion at user boundaries

§4.6's rule is decided and not yet implemented: issue #868.

## 8. Relation to earlier decisions

- **ADR-0021** (second amendment of 2026-10-01): its call-site rule now holds
  at every site kind (§4); `knows` replaces the per-pass knowledge closures
  (§3). The first amendment's refusal of `current` and `key` for lazy objects
  is revisited under §4.5's attribution, separately.
- **ADR-0040** §2: the four sources of throw facts are read off the site
  record; a known name with no throw row is a gap, never throwless (§3.2).
- **ADR-0046** (amendment of 2026-09-26): `eval` and an inclusion make both
  lanes non-exhaustive.
- **ADR-0067**: both lanes read the same receiver; the throw lane keeps a gap
  for a declared receiver until a bound for it is decided (an interface's
  `@throws` cannot bound the `Error` family).
- **ADR-0055** (amendment of 2026-09-26): state constructs stay gaps until
  their labels land; operator sites follow §4.
- **ADR-0063**: untainting edges keep their current reach over gaps (§5.2).
- **ADR-0084** §3 keeps tolerances away from spelling-producing sites; §5.2
  applies the same reasoning to gaps by analogy.
- The glossary's **Dischargeable obstacle** (ADR-0049 A14; ADR-0046 §2 and
  ADR-0047 use "obstacle" for transform enumeration) is a different notion —
  a silence leg of a check family that a plugin may discharge. A coverage gap is a reason an effect or throw answer is
  incomplete; the two meet at `__get` and `__call`, which §4 models for the
  lanes and the absence family models for its own proofs.
