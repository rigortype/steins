# A place, not a variable, names a heap entity: the array element carrier

**Status: proposed (2026-09-21), PENDING ratification.** Written to close
ADR-0097 §3's first deferral — "arrays of resources need a shape field that
holds a heap identity" — and the object half of the same hole (issue #675).
ADR-0036's heap and ADR-0097 §2.3's handle entry stand unchanged; what
changes is only *what may name* an entry.

The owner's framing: `resource` is claimed as a type as of the ADR-0097
series, and that claim has to be true of an array element too. `proc_open()`'s
`$pipes` is how real code holds a handle, and an analyzer that answers for
`$h` and shrugs at `$pipes[0]` has told half a truth.

## 1. Context

### 1.1 What is measured today: silence, not a wrong answer

Run against the current binary (`--profile strict`), a file exercising
`proc_open()`'s `$pipes`, an array literal of handles, `stream_socket_pair()`,
an escaped array of handles and a property holding a handle reports **nothing
from the resource families** — the only finding is an unrelated
`untyped.iterable-value` on a bare `array` parameter. So this ADR adds
missing true positives and a missing lane; it fixes no false positive, and
nothing here is a correctness emergency. What it is, is a claim the product
makes and does not keep.

### 1.2 Why an element cannot hold a handle

Three facts, each deliberate, compose into the hole:

1. **The value domain has no object and no resource.** `Val` is
   int/float/string/bool/null/array and `Base` is the four scalars
   (`crates/steins-domain/src/value.rs`). ADR-0035/0038's standing refusal
   keeps it that way, and ADR-0097 §3 restates it: there is no concrete
   resource value to carry.
2. **A shape field holds a `Fact`.** `ShapeFact::fields` is a `Vec<Field>`
   over the same scalar domain, so an element's slot can say `int` and
   cannot say *this handle*.
3. **Only a variable name reaches the heap.** `Store::refs` is
   `HashMap<String, AllocId>` keyed by a variable name, and every accessor —
   `res_of`, `obj_of`, `class_of`, `is_exact` — takes `&str`. A property is
   no better off: `HeapObj::props` holds `PropFact` *values*, not ids, which
   is why storing a handle into `$this->stream` forgets its state.

The identity is therefore not missing from the heap; it is missing a *name*.

### 1.3 What PHP does, probed at 8.5.10

- `stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, 0)` returns a
  **list of exactly two** `resource (stream)` handles, or `false`.
- `proc_open()` writes `$pipes` with **one entry per `pipe` descriptor**,
  keyed by that descriptor's key in the spec — `[0 => ['pipe','r'], 2 => ['pipe','w']]`
  yields keys `0` and `2`. A `file` descriptor produces **no** entry:
  `[0 => ['file', …], 1 => ['pipe','w']]` yields the single key `1`. The key
  set is a function of the spec argument, not of the arity.
- Each such entry is `resource (stream)`, open, and closed by the ordinary
  closers — `fclose($pipes[0])` closes the handle the element holds, and the
  element keeps holding the closed handle.

## 2. Decision

**A place names a heap entity.** The `Store`'s binding key stops meaning "a
variable name" and starts meaning "a place":

```
Place ::= <var>                the variable, as today
        | <var>[<proven key>]  one array element of it
```

spelled with the rendering the syntax layer already uses for an offset
(`ArgValue::render` gives `$bag[0]`, `IssetOperand::Offset` the same). A PHP
variable name cannot contain `[`, so the two namespaces cannot collide, and
every existing binding keeps its current key unchanged.

### 2.1 Why the key and not a new carrier in the domain

Putting an identity in `ShapeFact` would push the heap into a layer that is
deliberately scalar-only, and would make the shape algebra mutually
recursive with the heap for one spelling — the same argument ADR-0062 §3
makes for keeping the array stratum out of the scalar union. Keying the
*heap* by a place instead leaves the domain untouched.

It also costs almost nothing at a join. `join_stores` already keeps a ref
"only when every branch binds it to the SAME allocation id", and joins a
surviving handle's state over the three-point lattice. An element place is
just another key in that map, so `if ($c) { fclose($pipes[0]); }` merges to
`Unknown` by the rule that is already written, with no new merge code.

### 2.2 What binds a place

- **An array literal over a bound handle**: `$bag = [$h]` binds `bag[0]` to
  the id `h` holds. The *same* id, not a copy — PHP's handle semantics, and
  what makes `fclose($bag[0])` close `$h` (ADR-0097 §2.3's aliasing rule, one
  level out).
- **A producer's out-parameter**: `stream_socket_pair()` binds `pair[0]` and
  `pair[1]` to two fresh `Open` stream handles once the `=== false` guard has
  run; `proc_open()` binds one per `pipe` descriptor of a **proven** spec
  array, at that descriptor's key. An unproven spec binds nothing.
- Nothing else. An element written later (`$bag[1] = fopen(…)`) is out of
  scope for the first slice and named in §4.

**Amendment (2026-09-21): a key is `<proven key>`, not `<constant key>` — and
the floor is `Stratum::Verified`.** This ADR was drafted saying "constant key",
and §3 read that as "literal in the source". Slice 1 shipped a resolution that
threads the env, and the two together were unsound in the one direction that
matters: `Cx::resolve_literal` discards the stratum, so

```php
/** @phpstan-assert 0 $i */ function assert_zero(int $i): void {}
$arr = [$h, $g]; fclose($arr[0]);
$i = rnd(); assert_zero($i);   // a CLAIM — the body is empty
fread($arr[$i], 1);            // $i is 1; this reads the OPEN handle
```

reported `type.argument-mismatch` while the file printed `ok` and exited 0
(measured at 8.5.10 against the slice's own binary). A key does not merely
*describe* the subject of a judgment, it **selects** it, so an unverified claim
was picking which allocation the closed-state cell read — a false positive on
the default surface, which §2.3's own bias exists to prevent.

The rule is therefore the stratum rule this analyzer uses everywhere else, not
a syntactic one: **a key the walk has proven at `Verified` names a place.**
`$i = 0; fclose($arr[$i]);` is a proof and names `arr[0]`; `assert_zero($i)` is
a docblock claim and names nothing. This is the same floor `offset_operand_fact`
puts on an offset key one seam over — so §3's "the same rule the offset families
already use" becomes true of the code rather than aspirational — and the same
floor `check_resource_position` already put on the argument's *value*. A
literal-only rule was the alternative; it was refused because it would make
"proven" mean one thing for a value and another for a name, in one function.

One asymmetry is left standing, deliberately: the **effect** seam
(`resource_call_effects`) has no env threaded, so it stays literal-key only and
`fclose($arr[$i])` moves no state even where `$i` is proven. That loses
transitions and cannot invent one — a place is `Closed` only where a literal-key
close put it there — so it is a false negative, not a false positive, and
threading an env through that seam is its own change.

### 2.3 What unbinds one

Every path that can make the base stop holding what it held must drop the
element places under it, and a miss here is the failure mode that matters:
a stale `Closed` at a place the code has since rebound would be a false
positive on the default surface. The sweep points, each a place where a
variable is already invalidated today:

- `Store::unbind(var)` — also removes every key with the prefix `var[`.
- `Store::clear()` — unchanged; it drops everything.
- A write to the base under a **non-constant** key, a spread, a by-reference
  pass of the base, an escape of the base, or any unknown call that could
  reach it: the whole `var[` family drops. A narrower rule (drop only the
  written key) is not taken in the first slice, because "which key did that
  write touch" is exactly the question a non-constant key cannot answer.
- A `foreach` over the base, `array_shift`/`array_splice`/`sort` and the rest
  of the rearrangement family: they move elements between keys, so the
  family drops.

The bias is uniform: **when in doubt, drop the place**. A dropped place is
silence, which is the outcome the family already has today.

### 2.4 What reads one

`ArgValue::OffsetRead { base: Var(v), key }` with a key the walk resolves to a
`Key` **at `Stratum::Verified`** (§2.2's amendment) is the place `v[k]`. Every
consumer that today asks
`store.res_of(var)` asks it of that place instead, which is one resolution
function and no new judgment: the resource-ness cell (ADR-0097 §2.5), the
closed-state cell, and `is_resource()` narrowing all work unchanged once the
place resolves.

## 3. What stays out, and why

- **Deeper nesting** (`$a[0][1]`). One level answers `$pipes[0]` and every
  producer this ADR seeds; a recursive place needs a recursive sweep, and it
  buys nothing measured.
- **Unproven keys** (`$pipes[$i]` where nothing proves `$i`). No place, no
  binding, silence — the same rule the offset families already use.

  **Amendment (2026-09-21):** this bullet read "Dynamic keys (`$pipes[$i]`)"
  and described the *syntax* of the key, which neither §2.4's implementation
  nor the offset families it appeals to ever did — `offset_operand_fact`
  resolves a `Verified` variable key and `offset.missing` fires on `$a[$i]`
  accordingly. What stays out is a key the walk cannot prove, of which a
  docblock-`Asserted` one is a case; see §2.2's amendment for the measurement
  that forced the distinction.
- **Properties** (`$this->stream`). The identical hole, one carrier away:
  `Place ::= <var>-><prop>` is the same change in the same map, and it is the
  intended next increment rather than a separate design. Held back only to
  keep the first slice's sweep surface small.
- **`get_resources()`**. Its list has no statically known length or keys; the
  arm lane can say "array of resources" one day, but no place is nameable.
- **An object element** (issue #675's `min($dates)`, `array_pop($objects)`).
  The carrier is the same and lands with it; what stays out is the *value*
  question those issues also ask (an element's class in an aggregate), which
  is the arm lane's, not the heap's.

## 4. Slices and the gates

1. **The place key** — `Store` binding, lookup and the §2.3 sweep, with the
   array-literal binding site and the `OffsetRead` resolution. Gate: the
   existing resource suites unchanged, new pins for each sweep point, and
   fp-gate — a place that outlives its base is a false positive, so the
   sweep pins are the slice.
2. **The producers** — `stream_socket_pair` (a fixed two-element list) and
   `proc_open` (keys read off a proven spec, `pipe` descriptors only), with
   the `out_params` row `proc_open` does not have today. Gate: conformance
   unchanged, fp-gate, and a probe transcript per producer.
3. **Properties** (§3's second bullet), once the sweep has run on a corpus.

Each slice is silent until the one before it lands: a place that nothing
binds is a key that is never looked up.
