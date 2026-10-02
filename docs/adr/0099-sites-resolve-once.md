# A body's sites resolve once: reachable code, implicit user code and coverage gaps

**Status: accepted 2026-10-02, owner-ratified.** Designed autonomously
under the owner's standing delegation and ratified after the run landed.
Tracking issue #865; slices #861, #862, #863, #864, #858, #859.

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
   | ToString | `.`, `.=`, interpolation and heredocs, `(string)`, `echo`/`print`, a loose or ordering comparison (`==`, `!=`, `<`, `<=`, `>`, `>=`, `<=>`, `switch`) with an operand not shown object-free at any depth | the operand is object-free; or an exact class's closed chain has no `__toString`, which for a comparison holds only against an operand shown object-free at any depth | an exact class declares `__toString`, or a bound class's is final | gap |
   | MagicProp | a property read, write, `isset`, `empty`, `unset`, `??`, `??=`, a reference to it | the operand is not an object; or the class is exact, its closed chain declares no `__get`/`__set`/`__isset`/`__unset` and hooks no property of that name | an exact class declares the magic method or the hook | gap (§4.4 for a bound class's declared property) |
   | ArrayAccess | `$x[k]` read, write, `isset`, `empty`, `unset`, `??`, `??=`, a reference to it, list destructuring | the operand is not an object, or an exact class is proven not `ArrayAccess` | an exact class is `ArrayAccess`, or a bound class's `offset*` methods are final | gap |
   | Iterate | `foreach`, `yield from`, spreading a non-array | the operand is not an object, or an exact class's closed chain is not `Traversable` and hooks no property | an exact `Iterator`'s methods, or an exact `IteratorAggregate`'s `getIterator` together with the methods of the iterator it is shown to return (a `Generator`, which is final; an `ArrayIterator` only if unsubclassed, which no table states, so it is a gap); a bound class's when final | gap |
   | Clone | `clone`, and `clone` with a property list | an exact class's closed chain has no `__clone`, no `__set` where a property list is given, and hooks no property | an exact class declares `__clone` (or `__set`), or a bound class's is final | gap |
   | Call | a method absent from, or inaccessible in, a complete chain on a class declaring `__call`/`__callStatic` | — | an exact class, or a bound class's magic method is final | gap |

   A variable is never taken to hold no object on the frame's writes alone: a
   named call may take it by reference and store an object into it, so the
   resolver asks `Frame::held` for every variable operand.

   In an instance frame, `parent::m()` and `Foo::m()` for an `m` the chain
   lacks run `$this`'s own `__call`, which a subclass may override: the Call
   row's edge there is the enclosing class's, and only where that class or its
   `__call` is final.

   A chain that ends at an engine class is closed for a property access except
   at `ArrayObject` and `ArrayIterator` (and what extends them): with
   `ARRAY_AS_PROPS` a property fetch is an offset access, which a subclass's
   `offset*` answers, so such an ancestor opens the chain.

   An intermediate fetch under `isset`, `empty`, `??` or `??=` runs `__isset`
   (for an offset, `offsetExists`) before the read.

   A comparison whose two operands may both be objects is a gap whatever their
   classes: two objects of one class compare property by property, recursively
   through arrays, and any property pair may convert an object to a string
   (witnessed on PHP 8.5). Objects of two different exact classes are
   uncomparable and run nothing; the rule does not yet draw that distinction.
   For a `switch` the pairs are the subject against each `case`.

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
   closed; it runs nothing otherwise. A subclass that imports a trait counts,
   since a trait's body is not lowered, and so does an anonymous class
   extending or implementing the bound class or anything under it, since no
   index lists one (ADR-0049 A4). A subclass outside the universe is
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
| engine methods and constructors | #858 | measured: 4 bodies become `…?` in the effect lane and 5 in the throw lane, none completed; one `effects-envelope` tag is withdrawn. A `sprintf` value is an unproven shape, so `new \RuntimeException(sprintf(…))` in a coercive file gains a gap: that is 3 of the 4 |
| operator sites | #859 | measured: 703 bodies become `…?` in the effect lane (571 directly, 132 inherited) and 998 in the throw lane; 129 `effects-envelope` tags are withdrawn; no proven label moves and `check --profile strict` is byte-identical; the largest sole causes (measured before the comparison and subclass-shape rules, which add about 40 direct bodies) are call results and array elements as unproven operands (195) and trait-using classes, whose chains are open (105). The textual estimate was about 340 of 6,606 exhaustive bodies directly, before operand proofs (concatenation 134, non-`$this` property access 90, `foreach` 75, interpolation 47, cast 17, loose equality 17, `echo` 7, `unset` 4, `clone` 3) |

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

### 7.6 Implicit conversions and hooks outside §4.3's table

Three more places run user code through an operand and have no row: the
right-hand side of a string-offset write (`$s[0] = new S` converts the object
to a string); a dynamic property name that is an object (`(new P)->$n`,
`$$n`, `P::$$n`), which converts to the name; and a promoted constructor
property with a `set` hook, which runs the hook on the constructor's
argument. Each is a gap that no site records today. They are deferred, not
decided.

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

## Amendment (2026-10-02): engine classes are keyed and resolved by their FQN (#871)

**Status: proposed 2026-10-02; pending ratification.** This amendment is the part of the run
tracked in #915 that decides how the engine's classes are named (slice S2, #871). Later slices of
that run add their own dated sections.

### §3, a fourth item: an engine class is a hierarchy row, keyed by its FQN

4. **An engine class is a row of the mined hierarchy under its fully
   qualified name.** The hierarchy (`hierarchy.toml`, ADR-0043 §3) keys every class, interface and
   enum php-src's stubs declare by its FQN, lowercased: `pdo`,
   `random\randomexception`, `dom\element`. The catalog rows (`method_effect_labels`,
   `method_throws`, `method_arg_reach`) are keyed by the same FQN, and `engine_exit` (the class a
   method call's or `new`'s chain leaves the project at) is gated on that membership, where it was
   gated on the name having no namespace.
   - **Why the gate changed.** "No backslash" was a proxy for "an engine class", true while the
     engine's classes were thought to be unnamespaced. They are not: PHP 8.5.11 declares 73
     namespaced php-src classes (`Random\*`, `Uri\*`, `Dom\*`, `Pdo\*`, `FFI\*`, ...). The proxy
     refused all of them, so `new Random\RandomException` was a gap however well its parent was
     rowed. The new gate keeps what the old one protected: a name the user's namespace made up
     (`App\PDO`, an unimported `PDO` inside `namespace App`) is no hierarchy row, so it is still no
     engine class and still refuses.
   - **The hierarchy is the pin's.** It is mined from the stubs at the pinned release tag
     (`php-8.5.11`), so a row is a class PHP 8.5 declares when its extension is built in, and a
     class only php-src's development branch declares (`Io\Poll\PollException`,
     `Openssl\OpensslException`, `StreamException`) is not a row: 31 rows the first mining read
     from the development branch are gone, and `new` of any of them is an unknown class, as for any
     name nobody declares. Membership is the pin's, not the running engine's: `new \SNMPException`
     is answered by its row on a build without ext-snmp, and `new \Uri\InvalidUriException` on an
     8.4 runtime, exactly as the catalog's function rows already are (`doctor` reports the skew;
     `require catalog-pin-match` refuses it). The lanes consult no sidecar; the runtime leg is #954.
   - **A constructor row is the default only where the constructor is `Exception`'s or `Error`'s.**
     Every engine `Throwable` whose constructor is its own was audited with
     `ReflectionClass::getConstructor()` and a probe of each, and the rest take the default
     (`__construct` throws nothing). The four: `ErrorException` raises nothing, whatever the
     severity, file or line; `FiberError` raises an `Error`, always; `SoapFault` raises a
     `ValueError` for a `$code` that is not a fault code (`['a']`, `''`), and
     `Uri\WhatWg\InvalidUrlException` one for an `$errors` array holding anything but
     `UrlValidationError`s. The last two are checks on an admitted argument's value, which §3.3
     gives a row, so each has one (`ValueError`) before the default. A Throwable added with a
     constructor of its own needs the same audit before it takes the default.
   - **The miner read the keys wrong.** `extract_hierarchy.py` recognised `namespace X {` and
     `namespace X;` on one line. `ext/random/random.stub.php` and `ext/dom/php_dom.stub.php` put the
     brace on the next line, so 41 classes were keyed in the global namespace
     (`randomexception`, `engine`, `text`, `node`, `comment`, `number`, `secure`), and every
     relative `extends` or `implements` inside a namespace was recorded unresolved. The miner now
     reads a namespace statement across lines and resolves a parent the way PHP does: a leading
     backslash is fully qualified, anything else is relative to the declaring namespace, and a
     relative parent that names no declaration is reported on stderr.
   - **Consequences.** The 41 bare keys are gone from the table, so `class.undefined` no longer
     treats `Text`, `Comment` or `Engine` as engine classes the engine may declare (the sidecar's
     boot-surface leg decides those, as it does for any class the catalog does not know).
     Throwables such as `Random\RandomException` take `Exception::__construct`'s effect, throw and
     reach rows, so a body that builds or throws one is exhaustive in both lanes. A namespaced
     engine class with no row (`Random\Randomizer`, `Dom\Element`) is a gap of the lane's
     missing-row kind, as a global one is. Row corrections move the is-a walk too:
     `FFI\ParserException` is an `Error` (through `FFI\Exception`), where it was recorded as an
     `Exception`.
   - **The table is held to the engine.** A test offers every row of `hierarchy.toml` to a live
     PHP (`ReflectionClass`) and requires its `getName()` to lowercase to the key and carry the
     stored casing. A row the engine lacks must say why, and only three things explain it: its
     stub's extension is not loaded, this PHP is an older minor than the pin, or it is one of two
     stub-only pseudo-classes (`PDO_PGSql_Ext`, `PDO_SQLite_Ext`, whose stub says "This is not a
     real class"). A row mined under a wrong key is declared at the tag under that same key, so it
     is absent with its extension loaded, which nothing explains. The converse, that every
     namespaced class PHP 8.5.11 declares from php-src is a row under its FQN, is a static test in
     the catalog.
   - **Left open.** `Dom\DOMException` is a class alias of the global `DOMException` (the stub's
     `@alias`), not a class of its own: `ReflectionClass::getName()` answers `DOMException`. The
     hierarchy has no row for it, and that is not harmless on master: the alias already causes two
     proven false positives, `type.argument-mismatch` for `new \DOMException` passed to a
     `\Dom\DOMException` parameter, and `throw.undeclared` for a `\DOMException` caught by
     `catch (\Dom\DOMException)`. A follow-up issue tracks it; it needs the two names to agree in
     the is-a walk and in catch matching, which this slice does not model.

## Amendment (2026-10-03): names that convert, offset-write values and property hooks outside an explicit write are sites (#880, #875) — PENDING ratification

**Status: proposed 2026-10-03; pending ratification.** This amendment is slice S4 of the run
tracked in #915. It closes §7.6 and amends §4.2, §4.3 and §4.4; the coercion at user boundaries
(§4.6, §7.5) and the destructors (§7.1) are other slices' sections and stay as they are. Every
witness below was run on PHP 8.5.11, in a file with `strict_types=1` and in one without: none of
the three conversions depends on the calling file's mode.

### §4.3, a Name row: the names and values the engine converts to a string

The ToString family gains two forms, both resolved by the family's existing rule
(`string_conversion`, no new gap kind): an operand shown not to be an object runs nothing; an
exact class with no `__toString` runs nothing; an exact class with one is an edge; a bound, an
unknown class or an operand nothing names is `operator-to-string`.

- **A name** is converted: a dynamic property name (`$o->$n`, `$o->{$e}`, `$o?->$n`, as a read, a
  write, `isset`, `unset`), a variable-variable name (`$$n`, `${$e}`, `$$$n`) and a static
  property's name (`P::$$n`). The operand is the name expression. A dynamic *method* name is not
  one: `$o->$n()` with an object `$n` raises `Error: Method name must be a string`, and it is a
  dynamic callee already (§4.1). `$$n` is lowered by the site scan on the variable node, so every
  context that carries one (a read, an assignment target, `isset`, a static property's name) has
  the site. Reading `$$n` puts the frame on ADR-0001's give-up list, where no variable of the frame
  is shown to hold anything: `string $n` is not shown a string there, since `$$m = new S` can
  rebind it, and the name is a gap, as every other operator site over a variable of such a frame
  already is.
- **An offset write's value** is converted when the container is a string: `$s = 'abc'; $s[0] =
  new S;` runs `S::__toString` and stores its first byte. The rule is *not* "a string-offset write",
  which cannot be told from an array write syntactically (the container's shape is
  [`ArgShape::ObjectFree`] for a string and for an array of scalars alike). It is: a site over the
  value of every `$c[k] = v` (and every destructuring or `foreach` target `$c[k]`, whose value is
  unknown) whose value is not shown object-free and whose container is **not shown to hold no
  string**. A container is shown that way, and the site is ruled out, when it is a parameter whose
  declared type admits no `string`, `mixed` or `callable` (`array`, `?array`, a class, `iterable`,
  `int`), a local every whole-variable write of which is an array, `null`, a number, a boolean or an
  object (`$a = []`, `$o = new Foo`; never `'abc'`, a call, another variable, `.=`, a destructuring
  target or a `foreach` binding), or `$this->p` declared with such a type and not hooked, each only
  while no call of the frame may take the variable by reference. An `ArrayAccess` object is such a
  container: `offsetSet` receives the value and converts nothing (`ArrayAccess` is §4.3's own row).
  `$c[k] ??= v` is a site as `=` is: on a string it stores the value when the offset is unset
  and converts it (witnessed); the other compound forms (`.=`, `++`, `&=`) on a string offset
  raise an `Error` before converting, and an append `$c[] = v` is a fatal error, so none of them
  is one. A container that is an element (`$a['x']['y'] = $o`, where `$a['x']` may hold a
  string), a call result or another object's property is not shown, so it is a site.
- **Resolution.** `OperatorConstruct::Name` has one operand; `OperatorConstruct::OffsetValue` has
  two, the value and the container, and the resolver rules the site out through the container
  before it asks the family anything. A written local such as `$n = new S` is not an exact `S`: the
  walk is flow-insensitive and names a class only for a `new` written at the site, a final class or
  a never-written parameter, as for every other ToString operand, so `(new P)->$n` with `$n = new S`
  is `operator-to-string`, and so is the same shape with `new N`.

### §4.3, a promoted-hook site, and §4.2, hooked chains

- **A hooked promoted parameter is a MagicProp `Write` site on `$this`** in the constructor's own
  row, named by the parameter (`__construct(public string $p { set { … } })`; a `get`-only hook
  runs nothing at promotion and is no site), placed before the
  body's sites because promotion runs before the first statement. It resolves as an explicit
  `$this->p = …` does: the hooked property is `operator-magic-property`. The gap sits in the
  constructor's row and reaches every `new` and `parent::__construct` through the ordinary edge, so
  the constructor itself no longer reads exhaustive. A promoted parameter with no hook is not a
  site; the question of a *subclass's* hook over an inherited, unhooked promoted property is left
  open below.
- **A chain that leaves the project at an engine class and carries a hooking project class is a
  `operator-magic-property` gap beside the engine row** (§4.2's per-method rows are unchanged). The
  engine's constructors write the properties of their own class (`Exception::__construct` sets
  `$message`, `$code`, `$previous`) and its accessors read them (`getMessage()`), so a hook the
  subclass declares on any of them runs in the engine's code. It applies to `new Sub(…)` and
  `parent::__construct(…)` in both lanes, and to a method call that reaches an engine row in the
  effect lane (the throw lane's method calls have no row and are a gap already). Which property the
  engine touches is not read: any hook on the chain counts. The engine also sets `$file` and
  `$line` when it *creates* any exception or error, before and whatever its constructor, so a
  `new` of a project class that is (or may be) a `Throwable` and carries a hook on its chain is the
  same gap when its constructor is a project one too (an own empty constructor, a project parent's;
  witnessed with a hook on `$line`). For a `new` the class is exact, so the
  chain alone decides; for `parent::__construct`, `$this->m()`, `parent::m()` and a declared receiver
  the object is the enclosing class, a subclass of it or any class the declared type bounds, which
  §4.4's gate answers.

### §4.4, the universe gate through `is_a`

`subclass_adds_property_magic` asks whether `Chain::of(sub).has(class)`, and a chain never lists an
engine ancestor (`Throwable`, `Exception`), so for a receiver bound by an engine class the answer
was always no. The gate for a hook on any property is therefore asked of the supertype walk: some
class in `magic_property_classes` that hooks a property, or imports a trait (whose body is not
lowered), and may be the bound, or an anonymous class extends something that is. The bound is the
receiver's own class, not the engine class the chain exits at: `$this->getMessage()` in a
`LogicException` subclass is not charged for a hooking `RuntimeException`. A class that itself
hooks a property counts unless the walk shows it is not the bound, so one whose parent the universe
cannot read (a vendor parent) counts: its hook is user code whatever the parent is (witnessed with
`$e->getMessage()` on a `Throwable`). A class that only imports a trait, and an anonymous class,
count only when the walk shows they **are** the bound (`Yes`): counting an `Unknown` there charged
every site for every class whose parent a vendor-less checkout lacks (measured: it made 406
public-corpus bodies gain the kind, 5 once corrected), which is §7.2's open question.

### §7.6 closed, and what it leaves open

All three places §7.6 named are sites: the offset write's value, the dynamic names, and the
promoted parameter's hook. Left open, each witnessed:

- **A subclass's hook over an inherited, unhooked promoted property.** `class Sub extends Base {
  public string $p { set { echo '…'; } } }` with `Base::__construct(public string $p)` runs the hook
  at `new Sub('x')`. A promoted parameter with no hook is no site, so `Base::__construct` reads
  exhaustive. Lowering one MagicProp `Write` site for every promoted parameter would give the
  explicit write's §4.4 gate; the public corpora hold 4,554 constructors with 9,603 promoted
  parameters, none hooked, so the sites would cost payload and resolution for a gate that fires on a
  hook nobody there declares. Decided against here, not measured as a gap.
- **A hook a trait declares.** A trait's body is not lowered; a class importing one is counted by
  the universe gate and its chain is not closed, but a hook the trait declares on a property an
  engine class writes is not looked for in the trait itself.
- **A name in a write, `unset`, `++` or `&` position** (`$p->$n = 'w'` with `final class S` and
  `S $n`) is a gap where the read is an edge: the lvalue holds `$n`, which the declared-receiver
  gate counts as written. Sound, and left as it is.
- **A destructuring or `foreach` target that is an offset** is a site with an unknown value, which
  over-reports (`foreach ($rows as $a[$k])` over an array container the scan cannot show).
- **A hooking class declared twice.** A class the index cannot place, such as one declared in two
  files under `class_exists` guards, counts for the universe gate only where the walk shows it is
  the bound. `DupEx extends \Vendor\Base` with a `get` hook on `$message`, declared twice, runs
  that hook at `$e->getMessage()` on a declared `Throwable`, which reads exhaustive (a `new` of the
  class is already `unknown-class`). Counting every such class on `Unknown` costs 6,908 functions
  on a private project, because the magic-property set the gate reads cannot tell a hook from
  `__get`; a separate hooking-class set in the `symbols` shard would, at a schema bump (#993).
- **An abstract class's bodiless hook declaration** (`abstract public string $p { get; }`) counts
  as a hook though it runs nothing: conservative, a gap where PHP runs no user code.

### What moves (public corpora, `check --profile strict`, `--no-cache --no-php`)

On the ten public packages (28,847 functions) no body loses exhaustiveness in either lane (the 366
bodies that gain a kind were already `…?`), no proven label moves, `effect-diff` reports no event,
and the `effects-envelope`, `throws-envelope` and `loop-to-array-map` dry-runs are byte-identical per
package (723, 1,934 and 0 edits). (The counts in this paragraph predate the coalescing offset
write `$c[k] ??= v` added in review, which brings the bodies that gain a kind to 371, 366 of them
`operator-to-string`, and moves no strict finding.) 361 functions gain `operator-to-string` in both lanes: 202 carry the
construct in their own body (an offset write 187, a dynamic name 14, both 1, classified by reading
the body) and 159 inherit it through an edge. Five gain `operator-magic-property`: two `Chronos`
methods and `CarbonTimeZone::__construct` (an engine constructor or `createFromFormat` reached on a
`$this` whose class imports a trait) and two `writeError` methods that call `getMessage()` on a
declared `\Exception`, in a universe where a class importing a trait is an `Exception`. At `strict`,
`throw.maybe-undeclared` gains 50 findings, one per new (site, kind), and rewords 18 whose kind list
grew without a new site: 49 are `operator-to-string` (47 at an offset write, 2 at a dynamic name) and
1 is `operator-magic-property` (`parent::__construct` into `DateInterval`); the count rows of
`possibly_expected.toml` are reseeded by that delta. The public corpora declare no property hook, so
the hook rows move nothing there, and the one place a hook gap appears is the trait case above.
