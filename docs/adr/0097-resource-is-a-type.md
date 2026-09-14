# `resource` is a type: one runtime kind, an identity on the heap, a state the heap carries

**Status: proposed (2026-09-14), PENDING ratification.** Drafted at the
owner's direction to reorganize the `resource` semantics that ADR-0056 §8
and its §8.8 amendment accumulated one slice at a time. §8.1–§8.3 (why the
producer rows exist, the three-condition gate, the `Verified` grade) stand
unchanged. §8.4's carrier, §8.5's object refusal, §8.7's deferrals and the
whole of §8.8 are superseded where this ADR says otherwise.

The owner's framing, which every section below serves: PHP is retiring
`resource` — every release since 8.0 has turned another family of handles
into objects — and it is still, today, **a type of its own**: not
declarable in a signature, not a class, and not reducible to any of the
kinds Steins already models.

## 1. Context

### 1.1 What the type is, probed at 8.5.10

`resource` is the eighth runtime kind (`gettype()`'s universe is `null`,
`bool`, `int`, `float`, `string`, `array`, `object`, `resource`). A
resource value has three attributes PHP exposes: an **id**
(`get_resource_id`), a **kind** (`get_resource_type`: `stream`,
`stream-context`, `stream-filter`, `process`, … — `get_debug_type` spells
it `resource (stream)`), and a **state**, open or closed, that moves one
way only. Everything below was run on the pinned engine, and every cell
that matters to a verdict is recorded so no later reader has to recall it.

**Membership.** A resource is none of the other kinds: `is_scalar`,
`is_object`, `is_array`, `is_null`, `is_callable`, `is_iterable`,
`is_countable`, `is_numeric` all answer `false`, open or closed.
`is_resource` answers `true` for an open handle and **`false` for a closed
one**; `gettype` says `resource` and `resource (closed)`;
`get_resource_type` on a closed handle says `Unknown`; `get_resource_id`
still answers.

**Typed boundaries.** A resource satisfies **`mixed` and nothing else**, in
either coercion mode: `string`, `int`, `float`, `bool`, `false`, `null`,
`array`, `iterable`, `callable`, `object`, `Stringable`, `Closure`,
`int|string` and a property of type `int` all raise `TypeError: … must be
of type T, resource given`, with no `declare(strict_types=1)` in the file.
There is no `__toResource` and no resource-to-scalar coercion path
(ADR-0056 §8.5 already rests on this). A native `resource $x` hint is
**not this type**: PHP warns at compile time — `"resource" is not a
supported builtin type and will be interpreted as a class name` — and the
parameter then demands an instance of the class `Ns\resource`; a real
handle passed to it fails with `must be of type Ns\resource, resource
given`.

**Value-context conversions**, none of which is a boundary: `(bool)` is
`true` open or closed (so `empty($h)` is `false` and `!$h` is `false`);
`(int)`/`(float)`/`intval`/`sprintf('%d')` yield the id; `(string)`,
interpolation, concatenation and `sprintf('%s')` yield `Resource id #N`;
`(array)` yields `[$h]`; `settype` follows the casts. `==` against an int
compares the id, against another resource compares identity, against
`true` is `true`, against `null`/`[]`/an object is `false`; `<=>` orders by
id. **Operators refuse it**: `$h + 1`, `$h | 1`, `-$h` are `TypeError:
Unsupported operand types`; `count($h)` and `clone $h` are `TypeError`;
`$a[$h]` warns and casts to the id; `$h[0]` warns and reads `null`;
`foreach ($h …)` warns and iterates nothing; `json_encode($h)` is `false`
(`Type is not supported`); `serialize` gives `i:0;`; `var_export` prints
`NULL`.

**Identity and state.** A handle is shared by reference semantics: `$b =
$h; fclose($b)` leaves `$h` reading `resource (closed)`, and so does a
callee that closes its argument, and so does closing `$arr[0]` after `$arr
= [$h]`. `unset($x)` of an alias closes nothing; dropping the last
reference closes silently. A closed handle never reopens. A **closing
call** is a builtin that leaves its argument closed when it returns:
`fclose`, `pclose`, `closedir`, `proc_close`, `gzclose`, `bzclose`. Two of
them are kind-sensitive: `fclose` on an `opendir()` handle warns (`cannot
close the provided stream, as it must not be manually closed`), returns
`false` and **leaves it open**; `closedir` on an `fopen()` handle is a
`TypeError` (`must be a valid Directory resource`); `pclose` closes any
stream, `fclose` closes a `popen()` pipe, a `proc_open()` pipe and a
`tmpfile()`.

**Consumers.** 141 functions in the php-src stubs at the pin declare a
position `@param resource` (82 in `basic_functions.stub.php`, the `gz*`
and `bz*` families, `hash_update_stream`, `ftp_fget`, …). Reflection
reports **no type** for every one of them — the language cannot spell it —
and the engine checks at the call: a non-resource is `TypeError: fwrite():
Argument #1 ($stream) must be of type resource, string given` (`null`,
`stdClass` and an `SplFileObject` fail the same way, in either mode); a
closed handle or a handle of the wrong kind is `TypeError: fread():
Argument #1 ($stream) must be an open stream resource`, and a second
`fclose` is the same error. A few positions accept any state
(`get_resource_type`, `get_resource_id`, `gettype`, `is_resource`,
`stream_context_get_options` on a live stream or context).

### 1.2 What Steins says today

The type is carried in three places that do not know about each other, and
the seams between them are where the defects live.

- **Three carriers.** `ContractTy::Resource` in the contract lane (the
  declared type), `CVal::Resource` in `steins-infer` (the proven value),
  `RtKind::Resource` in the predicate kinds. No `Val` and no `Fact` is a
  resource, so a variable holding one has **only** an arm-lane entry, read
  by the proof layer through the §8.6 lock (one arm, `Resource`,
  `Verified`) and special-cased in four argument families.
- **The lane does not survive use.** Probed on `master` (`db1054c6`) after
  `$h = fopen(…); if ($h === false) { throw … }`: `strlen("x");`,
  `userfn($h);` and `$b = $h;` keep `$h` a `resource`; **`ftell($h);`
  erases it to `unknown`**, and so does `$x = fread($h, 1);`. A builtin
  receiving the handle is treated as a possible writer of it, so the
  handle is lost at its first use — before any stream function can be
  judged against it.
- **`is_resource` erases what it confirms.** `if (is_resource($h)) { … }`
  dumps `unknown` in **both** branches, and `if (!is_resource($h)) {
  throw … }` leaves `unknown` after it: the predicate is not a `TypePred`
  (§8.4 deferred it), and an unmodeled predicate over the lane drops the
  lane. The guard PHP code actually writes is the one that costs the fact.
- **Native `resource $x`** is reported as `class.undefined: reference to
  undefined class App\resource — not defined in the project, not on PHP
  8.5.10`. The diagnosis is PHP's own; the sentence tells the author to
  define a class.
- **Docblock `resource`** lowers to the leaf and, since #744, no class
  named `Resource` shadows it.
- **State** (#746, ADR-0056 §8.8) is a `ResourceState` on the contract
  type plus an unspelled `fclose_closes: bool` on the same type — a
  producer property carried by a type — and the proof reads the arm lane
  per variable, so `$b = $h; fclose($b); f($h)` is silent, and "open" is
  declared unprovable.
- **Consumers are silent.** `fwrite('x', …)`, `fclose(null)`, and the bug
  §8.7 named — `fclose($h); fread($h, 1)` — report nothing; ADR-0056 §9.4
  declines every untyped position, which is every resource position.
- **An object against `@param resource` is `Maybe`** (§8.5's named channel
  for the `CurlHandle` rot). Every other tool on the conformance page —
  PHPStan, Psalm, Phan, Mago, PHPantom — reports `acceptsResource(new
  \stdClass())`; Steins is the one that declines, and it declines for
  `stdClass`, which no migration ever produced.
- **Arrays of resources have no carrier**, which hides `proc_open`'s
  `$pipes` — the most common way a handle is obtained after `fopen`.
- **`gettype($h)` folds to `string`**, `get_resource_type` and
  `get_resource_id` to nothing.

None of these is a false positive; every one is a silence, and together
they are the shape of a type that was admitted to the analyzer without a
place of its own.

## 2. Decision

### 2.1 The universe: a leaf kind, partitioned by state and by kind

`resource` is a **leaf** in Steins' runtime universe, beside the scalars,
`array`, `object` and `null`: it has no hierarchy, no members and no
coercion path, so every verdict about a proven resource is exact. Two
partitions refine it and nothing else does:

| refinement | members | how Steins learns it |
| --- | --- | --- |
| state | `open-resource`, `closed-resource` | the heap (§2.3), a closing call (§2.4), an `is_resource` guard (§2.4) |
| kind | `stream`, `dir`, `process`, `stream-context`, `stream-filter`, … | the producer row (`fopen` → `stream`, `opendir` → `dir`, `proc_open` → `process`) |

`dir` is Steins' own kind name: PHP reports a directory handle as
`get_resource_type() === 'stream'` and then refuses to `fclose` it, so the
distinction the closing table needs is one the runtime keeps private. Kind
names are the `get_debug_type` spelling where PHP has one and this ADR's
where it does not; they never reach a docblock.

The **boundary table** is §1.1's, and it is closed: at a typed boundary a
resource satisfies `mixed` and no declared type, in either mode; every
value-context conversion is a conversion and never an acceptance. This is
the vocabulary every family reads — the argument and return relations, the
operator family (`type.invalid-operand` may consume `$h + 1` → `TypeError`
the day it reads this table; not this ADR's slice), the folds.

### 2.2 The spellings, and the one reading that is not this type

- **Docblock.** `resource`, `open-resource` and `closed-resource` lower to
  `ContractTy::Resource { state }`, `Any`/`Open`/`Closed`, and spell back
  as themselves. `subsumes` between two *declared* states is `Maybe`
  wherever they differ (`@return closed-resource` into `@param
  open-resource` is a dataflow question two docblocks cannot settle);
  `resource` covers both. The state qualifier is the only thing the
  contract type carries — #746's `fclose_closes` bit leaves it (§2.3).
- **No class shadows it.** `is_shadowable_pseudo_type("resource")` is
  `false` (#744, divergence-registry entry 19). PHP lets `class Resource
  {}` compile and PHPStan lets it shadow the pseudo-type; Steins reads the
  docblock word as the type unconditionally, because the docblock is the
  *only* spelling the type has and the class reading turns every handle
  the author meant into a proven non-instance.
- **Native `resource $x` is a class reference**, exactly as PHP compiles
  it, and stays `class.undefined` — the id is right, since the runtime
  will look for `Ns\resource` and not find it. The **message** changes to
  say what PHP says: *`resource` is not a type PHP can declare; this
  reads as a reference to a class `Ns\resource`, which is not defined —
  declare the parameter untyped and write `@param resource`.* The call
  site keeps its `type.argument-mismatch` (`holds a resource … cannot
  become Ns\resource`), which is what PHPStan, Psalm, Phan and Mago all
  report there too.

### 2.3 The carrier: the type in the arm lane, the identity and the state on the heap

A resource is a **handle with identity** — aliases share it, a callee can
close it, its one state change is a fact about the handle and not about a
variable. That is the object model's situation (ADR-0036), not the value
domain's, and the value domain stays resource-free (ADR-0035/0038): there
is no concrete resource value to enumerate, join or widen, and nothing a
`Val` could hold.

So the carrier splits along the line ADR-0036 already drew for objects:

- **The type** stays in the contract arm lane, seeded `resource` (plus
  `false` where the stub declares it) at `Verified` by the §8.2 gate, and
  read by the proof layer through the §8.6 lock. The lane is what answers
  "is this a resource"; `$h === false` subtracts the failure arm as it
  always did.
- **The identity and the state** live on the heap. A producer call
  allocates a heap resource `{ kind, state: Open, producer }` bound to the
  assigned variable, exactly as `new` allocates an object; `$b = $h` binds
  a second name to the same entry; a branch merge merges entries as
  object property facts merge (`Open ⊔ Closed = Unknown`, `Closed ⊔
  Closed = Closed`). `CVal::Resource { state }` is read off the heap, and
  the §8.6 lock gains a second clause: the binding refers to a heap
  resource. The proven value the argument families judge is therefore
  `(type from the lane, state from the heap)`.

`Open` at allocation is a proof, not a guess: `fopen` returned, `false`
was subtracted, nothing has touched the handle. What can invalidate it is
exactly what invalidates an object's property fact — an escape — and
§2.4 says which calls escape.

### 2.4 State discipline: the object escape rule, with a table of closers

The state of a heap resource changes only through the handle, and a
handle changes only when something receives it. So the rule is ADR-0036's
sweep, keyed on what received it:

| the handle is … | state after |
| --- | --- |
| passed to a **closing call** (`fclose`, `pclose`, `closedir`, `proc_close`, `gzclose`, `bzclose`) whose table row closes the handle's *kind* | `Closed` — the call returned, so every argument it rejects has already thrown |
| passed to a **keeper** — a stub-table position (§2.5) whose row says the call does not close | unchanged |
| passed to anything else: a project function, a method, a callback, a spread or by-reference position, a name the tables do not hold | `Unknown` (the escape) |
| stored into an array or a property, captured by a closure, returned | `Unknown` (the escape) |
| the subject of `is_resource($h)` | true branch `Open`, false branch `Closed` — on a lane that holds a resource arm; on `resource\|false` the false branch keeps `false` beside the closed handle |
| re-assigned | the binding leaves the entry; the entry keeps its state for its other names |

Three properties make this sound and are the reasons for the table's
shape. A closing call's **return is the whole premise**: `fclose` on a
non-resource, a closed handle or a wrong-kind handle is a `TypeError`, so
the statement after it runs only when the close happened — which is why
`fclose` over a `dir` handle, which warns and returns `false`, is the one
row where a closer is a keeper, and why the kind lives on the heap entry.
**Closed is monotone**: no call reopens a handle, so `Closed` survives any
escape and any join with `Closed`. And **`Unknown` convicts nothing**: a
handle in the unknown state is `Maybe` against both state spellings and
against every consumer position that demands an open one; only `Open` and
`Closed` are read.

This retires §8.8's two refusals for the reason §8.8 gave them. "Open is
never proven" was true of an arm lane keyed per variable, where `$b = $h;
fclose($b)` could not reach `$h`; on a heap entry the alias *is* `$h`, and
an unknown callee is an escape rather than an invisible close. The
`fclose_closes` bit was the kind, carried on the wrong noun.

### 2.5 Consumers: the parameter twin of the producer table

`resource_params.toml`, mined from the php-src stubs at the pin the way
`resource_returns.toml` is: one row per `(function, position)` whose stub
type is exactly `resource`, carrying the kind the position demands where a
probe established it (`stream`, `dir`, `process`, `stream-context`, or
*any*), whether the position accepts a closed handle (`get_resource_type`,
`get_resource_id`, `is_resource`, `gettype`), and whether the call
**closes** the handle (§2.4's closers are rows of this table, not a second
list). Positions whose stub says `resource|string` or `resource|null` are
out: the union is judged by the ordinary relation once the arms can be
lowered, and a curated union row would be the parameter refinement §9.6
refuses.

Admission is §8.2's gate in the parameter direction: the stub says
`resource`, **the engine declares no type for the position**, and the
minor is the pin. The second condition is the same tripwire it is for
producers — the day `fwrite` declares `Stream $stream`, the row is
disowned by the engine speaking — and reflection already reports the
position's declared type per §9.1, so the check costs nothing new. The
grade is `Verified` on §8.3's argument: the row and the engine can
disagree in one observable way, and every run observes it.

The judgment is §9.2's relation with two more cells, both mode-independent
because §1.1 measured them so: a proven non-resource at a resource
position is `type.argument-mismatch` (`must be of type resource, string
given`); a proven **closed** handle at a position that demands an open
one is the same id with the state named (`must be an open stream
resource`). A kind mismatch (a `dir` handle into `fread`) is the same
error and the same id, admitted per row only where the kind was probed.

### 2.6 An object against `@param resource`: `No`, except the migrated classes

§8.5 refused this verdict outright to protect a real channel: PHP 8 left
`@param resource $ch` on parameters that now receive a `CurlHandle`, and
the docblock is rot the caller inherited. The channel is real and it is
**finite**. At `mine-function-map` time the pinned functionMap says
`resource` for 110 names the engine knows and the engine declares an
object for 89 of them (ADR-0056 §8.2's measurement); the set of classes
those 89 declare — `CurlHandle`, `GdImage`, `LDAP\Result`, `Odbc\Result`,
`finfo`, … — is exactly the set of classes a `@param resource` may
legitimately be handed. It is derived from the disagreement the gate
already computes, versioned by the pin, and needs no hand-maintained
list.

So: an object whose class is in the **migrated table** stays `Maybe`
against `resource` (the docblock is the suspect, §8.5 unchanged for it);
any other object is `No`. `new \stdClass()` convicts, as it does under
every other analyzer; a `CurlHandle` does not. The day a `Stream` class
replaces `fopen`'s handle, it joins the table for as long as functionMap
lags the engine, and leaves it when functionMap catches up — at which
point `@param resource` handed a `Stream` is rot the docblock's own
vocabulary has abandoned, and PHPStan reports it too.

### 2.7 Folds that come with the kind

A proven resource folds `gettype` to `'resource'` (`Open`), `'resource
(closed)'` (`Closed`) or their union (`Unknown`); `get_debug_type` to the
kind spelling; `get_resource_type` to the kind (`'Unknown'` when closed);
`get_resource_id` to `int<1, max>`; `is_resource` to `true`/`false` by
state. Each is a one-row fold over §2.1's table, listed so the family is
complete rather than discovered.

### 2.8 The retirement path is the design, not a risk to it

Every table this ADR adds or keeps is admitted by the engine and switched
off by the engine:

- **producers** (§8.2): a declared return type disowns the row;
- **consumers** (§2.5): a declared parameter type disowns the row;
- **the migrated table** (§2.6): derived from the producers' disagreement
  with functionMap, so it grows with each migration and shrinks as the
  map catches up;
- **the spellings** (§2.2) never retire: `@param resource` is in the
  stubs, in every framework, and in a decade of code, and it will outlive
  the last handle.

Under `--no-php` nothing is admitted and nothing is tracked — the family
is engine-backed end to end (ADR-0004's sound subset, unchanged). On a
PHP that has migrated everything, every table is empty, the spellings
still lower, and an object handed to `@param resource` is judged by §2.6
alone.

## 3. What stays out, and why

- **A `resource` inhabitant of `Val` or `Fact`.** Standing refusal
  (ADR-0035/0038): there is no concrete value to carry, and the abstract
  question — "is this a resource" — is the arm lane's, with identity the
  heap's. Adding a base would put a non-scalar into every scalar table.
- **Arrays of resources.** `stream_socket_pair`, `get_resources`, and
  `proc_open`'s `$pipes` need a shape field that holds a heap identity —
  the same carrier an object element needs and does not have today.
  Deferred with that design named; it is the next slice, not this one.
- **Persistent streams** (`pfsockopen`) as a closer target: `fclose` on a
  persistent stream was not probed; the row stays a keeper until it is.
- **Method-keyed consumers** (`SplFileObject::__construct`'s
  `resource|string`, `Phar::setStub`): §4's function-keyed bound.
- **Operators.** §2.1's table says `$h + 1` is a `TypeError`;
  `type.invalid-operand` is the family that would say so, and reading the
  table there is its own slice.
- **Proving a handle open across a project call.** An escape is an
  escape; a summary that proves a callee does not close its argument is
  the effects lane's question, and the state falls out of it if it ever
  answers.

## 4. Consequences for the open pull requests

- **#744** (`int-mask`, `non-empty-literal-string`, the shadowing rule) is
  unchanged; §2.2 restates its rule.
- **#745** (`key-of<T>` at a call) is untouched.
- **#746** (`closed-resource`) is reshaped, not discarded: `ResourceState`
  on the contract type stays as the spelling qualifier; `fclose_closes`
  leaves the type for the heap entry's kind; the proof moves from
  re-inserting an arm to setting the heap state; and its verdict tests
  keep passing while the ones §2.4 enables are added — the alias close,
  the `is_resource` true branch, a fresh handle against
  `closed-resource` (`No`, now that `Open` is proven), and the lane
  surviving `ftell($h)`.

## 5. Slices and the measurements that gate them

1. **Heap identity and state** (§2.3, §2.4): the entry, the binding, the
   closers table read from the stub rows, `is_resource` as a `TypePred`,
   and a builtin keeper no longer erasing the lane. Gates: the #746 tests
   plus the four above; conformance `closed_resource` 1/2 → 2/2,
   `open_resource` stays 2/2; fp-gate flat.
2. **The consumer table, resource-ness and closed state** (§2.5): the
   mining, the tripwire, `fwrite('x')` and `fclose($h); fread($h)`.
   Gates: fp-gate with verbatim triage of every new row — this is a new
   true-positive family on the default surface.
3. **The migrated table** (§2.6): derived at `mine-function-map`, consumed
   by the contract relation. Gate: conformance `resource` 2/3 → 3/3;
   fp-gate — the monolog and symfony/console rows already record the
   negative-test shape this will land on.
4. **The message and the folds** (§2.2, §2.7). Gate: the
   `native_types_resource_argument` output unchanged in ids.
5. **Kinds per position** (`dir`/`process`/`stream-context`), probed one
   row at a time, and the arrays-of-resources carrier once a shape field
   can hold an identity.
