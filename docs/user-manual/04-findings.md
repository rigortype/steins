# Findings

CI just failed and printed a line you have not seen before. This chapter
tells you how to read it, which of Steins' four claim layers it comes from,
and what the analyzer had to prove before it was allowed to say anything.

Start with the line itself:

```
src/Native.php:19:8: error[type.argument-mismatch]: argument "1200" to charge() cannot become int $cents — proven TypeError (strict mode)
```

That is one finding. Every line Steins prints has the same shape.

## Reading one line

```
path:line:column: level[id]: message — tail
```

**Path, line, column.** The path is relative to where you ran `check`. The
column anchors the sub-expression the check judged, which is usually
narrower than the whole statement — here it lands on the `"1200"` argument
inside the `charge(...)` call.

**Level.** `error` or `warning`. `error` means the finding is fail-level and
the run exits `1`. `warning` means report-without-fail and, on its own, exit
`0`. A profile's `warn = [...]` list is what demotes an id from one to the
other, and `--format json` spells the same two levels `fail` and `warn`. See
[profiles, baseline, and suppression](05-profiles-and-baseline.md) for the
config side.

**The id.** `type.argument-mismatch`, in `family.rule` form. The id names the
*finding*, never the code that found it, so an id survives a rewrite of its
emitter (ADR-0022). It is also the only handle you get: ids are what
`@steins-ignore` accepts, what profiles select over, and what the baseline
records. There is no message-text matching anywhere in Steins.

**The message.** What Steins found, in the vocabulary of your source. It
names the value it folded (`"1200"`), the target it judged against
(`int $cents`), and the callee. Message wording is not a contract and keeps
improving.

**The tail.** The clause after the em dash, and the part worth reading
twice. It is Steins showing its work: either the runtime consequence it
proved, or the evidence that closed the case. `proven TypeError (strict
mode)` is a consequence. `hierarchy fully enumerated (Mailer), no __call` is
evidence — the reason the analyzer was entitled to claim absence rather than
stay quiet. Mechanics and debug lines carry no tail; they are not proofs.

The same tail records *why* the same code is silent elsewhere. Take the
`declare(strict_types=1)` away and pass both a numeric string and a
non-numeric one:

```php
<?php

function charge(int $cents): void {}

charge("1200");
charge("twelve");
```

```
$ steins check src/Coercive.php
src/Coercive.php:6:8: error[type.argument-mismatch]: argument "twelve" to charge() cannot become int $cents — proven TypeError (coercive mode)
```

Only the second call reports. `charge("1200")` works under coercive mode, so
Steins stays quiet about it; `charge("twelve")` still fatals, and the tail
now says `coercive mode`. That is the whole posture in one file: "the
program works" outranks the worst-case static reading (ADR-0002).

> **If you know PHPStan or Psalm:** the id is PHPStan's *identifier*, and it
> carries all the weight here that the error message carries there. Steins
> has no `ignoreErrors` with a `message:` regex and never will — diagnostic
> wording is not a contract, so ids plus semantic scope are always the
> substitute (ADR-0023). Grep your CI logs for `[` and you have the whole
> vocabulary of a run.

`--format json` emits the same finding structured, with the layer made
explicit:

```
$ steins check --format json src/Coercive.php
{
  "findings": [
    {
      "id": "type.argument-mismatch",
      "layer": "proof",
      "level": "fail",
      "path": "src/Coercive.php",
      "line": 6,
      "column": 8,
      "message": "argument \"twelve\" to charge() cannot become int $cents — proven TypeError (coercive mode)"
    }
  ],
  "profile": "default",
  "vendor_suppressed": 0,
  "suppressed": 0,
  "baselined": 0
}
```

## The four layers

Every id carries a **layer** as a registry attribute, and the layer names
what *kind of claim* the finding makes. It is not a severity dial (ADR-0050).

**proof** — your program provably breaks on a live path. A `TypeError`, an
`Error`, an `ArgumentCountError`, a warning-and-null read. Held to a
zero-false-positive bar. If a proof finding is wrong, that is a bug in
Steins, not a tuning problem.

**contract** — a proven behavior violates something your code *declares
about itself*: a `@param`, a `@return`, a `@throws`, an effect envelope, an
array shape. The program still works. These findings are true and abundant
in released code, which is why they are opt-in.

**mechanics** — the analyzer's own hygiene. A stale `@steins-ignore`, a
misspelled id, a typo'd effect label. Their absence would silently rot
another channel, so they print in every profile and no suppression channel
reaches them.

**debug** — requested introspection (ADR-0053, ADR-0074). You wrote
`dumpType()`, a `@psalm-trace` docblock, or `var_dump()`; Steins answers. A
dump is an answer, not a finding, so the layer sits outside the profile
ladder and outside every gate.

Which profile puts each layer on the surface:

| Layer | `default` | `throws-direct` | `contracts` | `strict` | `pedantic` |
| --- | --- | --- | --- | --- | --- |
| proof | yes | yes | yes | yes | yes |
| mechanics | yes | yes | yes | yes | yes |
| contract | no | `throw.undeclared`, direct escapes only | all but the strict and pedantic rungs | contracts + the some-paths-only claims | contracts + the house-style asks |
| debug | yes | yes | yes | yes | yes |

`strict` and `pedantic` both build on `contracts` and neither contains the
other — see [chapter 5](05-profiles-and-baseline.md).

`steins doctor` prints the resolved surface for your build and config, which
is the authoritative answer for a given binary — two lines out of its
`Config + active surface` section:

```
$ steins doctor --no-php .
  active profile: `default` (from built-in default)
  surface: layers [mechanics, proof], 47 checked id(s)
```

Today that count runs 47 ids at `default`, 48 at `throws-direct`, 63 at
`contracts`, 69 at `strict` and 64 at `pedantic`. The profiles, the baseline
ratchet that makes raising one survivable, and user-defined profiles all live in
[chapter 5](05-profiles-and-baseline.md). The normative rules for layers,
facets, and suppression are in
[diagnostic-policy.md](../type-specification/diagnostic-policy.md).

> **If you know PHPStan or Psalm:** a profile is not a level. Moving from
> `default` to `contracts` admits a different *kind* of claim, one the
> engine always computed and always withheld; the checks themselves do not
> get more aggressive. Nothing about inference changes when you raise a
> profile (ADR-0050 §10).

## The catalogue

The registry holds **73 ids**, 72 of them with a live emitter. It is a closed
set bound by a totality test, so an id that reaches your terminal is in it
and an id outside it cannot be emitted (ADR-0022). Each id below is shown
with the PHP that triggers it and the transcript it produces.

**The catalogue covers eleven families and is behind the registry.** v0.1.4
landed a large port wave — `property.*`, `variable.*`, `constant.*`,
`class-const.*`, `override.*`, `string.*`, `preg.*`, `array.*`, `closure.*`
and `syntax.*` — and those families have no section here yet. `steins doctor`
is the authoritative answer for your build; this page is a guide, not a
census.

Transcripts are from the v0.1.2 binary against PHP 8.5.8 except where a
section says otherwise; the `untyped.*` transcripts were produced on the
current build.

### `type.*` — native declared types, proven

Four ids. The first three are proof layer on the default surface: each fires
only when a folded value provably raises a `TypeError` against a *native*
declaration under that file's own coercion mode. The fourth
(`type.maybe-argument-mismatch`) is the same question asked one notch weaker,
about a type rather than a value, and reaches only `strict`.

```php
<?php

declare(strict_types=1);

final class Invoice
{
    public int $total = 0;

    public function tax(): int
    {
        return "0.08";
    }
}

function charge(int $cents): void {}

$invoice = new Invoice();
$invoice->total = "1200";
charge("1200");
```

```
$ steins check src/Native.php
src/Native.php:11:16: error[type.return-mismatch]: return "0.08" cannot become int (return type of Invoice::tax()) — proven TypeError (strict mode)
src/Native.php:18:1: error[type.property-mismatch]: Cannot assign "1200" to property Invoice::$total of type int — proven TypeError (strict mode)
src/Native.php:19:8: error[type.argument-mismatch]: argument "1200" to charge() cannot become int $cents — proven TypeError (strict mode)
```

`type.argument-mismatch` judges an argument against a parameter,
`type.return-mismatch` a `return` expression against a return type,
`type.property-mismatch` an assignment against a property type. The handbook's
[type system chapter](../handbook/02-the-type-system.md) covers what "cannot
become" means for each PHP type.

**`type.maybe-argument-mismatch`** is the fourth, and the odd one out: proof
layer, but reaching only `strict`. It fires where the *type* an argument
carries has an arm the parameter rejects **and** an arm it accepts. Nothing
here is proven to break — a type says what a value may be, not what it is —
which is why it sits on the opt-in rung and not the default surface.

```php
<?php

declare(strict_types=1);

function shorten(string $path, int $max): string
{
    return substr($path, 0, $max);
}

function shortAbsolute(string $path, int $max): string
{
    $resolved = realpath($path);

    return shorten($resolved, $max);
}
```

```
$ steins check --profile strict src/Paths.php
src/Paths.php:14:20: error[phpdoc.maybe-argument-mismatch]: argument $resolved to shorten() may not become string $path — $resolved is non-empty-string|false, and its false arm raises a TypeError (strict mode)
```

`realpath()` returns `string|false`, and handing that straight on to a
`string` parameter works until the day the path does not resolve. Guard it
and the finding goes away:

```php
$resolved = realpath($path);
if ($resolved === false) {
    throw new \RuntimeException("cannot resolve {$path}");
}
```

`assert($resolved !== false)` discharges it too, as does any `!== false` /
`!== null` / `instanceof` guard the branch reaches through, and a
`@phpstan-assert` tag that subtracts the arm.

The id comes in two spellings, and which one you get says where the evidence
came from — the same split `call.undefined-method` and
`phpdoc.undefined-method` make. **`phpdoc.maybe-argument-mismatch`** (contract
layer) means at least one arm came from a docblock or from a builtin's
declared return, so the claim is conditional on that declaration being
honest — the `realpath()` case above. **`type.maybe-argument-mismatch`**
(proof layer) means every arm came from a native declaration PHP itself
enforces:

```php
function shortHome(string|false $home, int $max): string
{
    return shorten($home, $max);
}
```

```
src/Paths.php:22:20: error[type.maybe-argument-mismatch]: argument $home to shorten() may not become string $path — $home is string|false, and its false arm raises a TypeError (strict mode)
```

Both reach `strict` and neither reaches `contracts`, because both make a
*may* claim. Note the mode in the message: the judgment runs under the calling
file's own `strict_types`. A `?string` into an `int` parameter fires in
coercive mode — the `null` arm breaks, a numeric string does not — and says
nothing in strict mode, where *every* arm breaks. That last silence is
deliberate: "every arm breaks" is a claim about the declaration rather than
about any value on any path, and it was measured to fire on nothing real.

Two limits worth knowing. Arguments to **builtins** are not checked against
builtin parameter types at all — Steins has no builtin parameter-type source
— so `strlen($maybeFalse)` is silent. And only a plain `$variable` argument is
read: `f($o->prop)`, `f(g($x))` and `f($a['k'])` carry the same risk and are
not judged here.

### `call.*` — calls that cannot complete

Six ids, proof layer, default surface. The family splits into three
questions: is the receiver alive, does the target exist, do the arguments
bind.

**`call.on-null`** — the receiver is proven `null` on this path, so the call
is a guaranteed `Error`. Only a proven `null` fires. A value that merely
*might* be null stays silent, which is the single most common reason Steins
says nothing about code you expected it to flag (see the handbook's
[narrowing chapter](../handbook/03-narrowing-and-trust.md)).

```php
<?php

declare(strict_types=1);

function stamp(?DateTimeImmutable $at): string
{
    if ($at === null) {
        return $at->format('c');
    }

    return $at->format('c');
}
```

```
$ steins check src/OnNull.php
src/OnNull.php:8:16: error[call.on-null]: method call $at->format() — $at is proven null on this path — proven Error (Call to a member function on null)
```

**`call.undefined-method`** and **`call.undefined-function`** — the target
does not exist. Absence is hard to prove, and the tail tells you what closed
it: a fully enumerated hierarchy with no `__call` for the method, and the
live PHP's own answer for the function.

```php
<?php

declare(strict_types=1);

final class Mailer
{
    public function send(string $to): void {}
}

$mailer = new Mailer();
$mailer->sendMail("ops@example.com");

echo str_slugify("Hello World");
```

```
$ steins check src/Absence.php
src/Absence.php:11:1: error[call.undefined-method]: call to undefined method Mailer::sendMail() — hierarchy fully enumerated (Mailer), no __call
src/Absence.php:13:6: error[call.undefined-function]: call to undefined function str_slugify() — not defined in the project, not on PHP 8.5.8 (70 extensions)
```

`70 extensions` is not a static table. Steins asked the PHP on your `PATH`.
Run with `--no-php` and this family goes quiet, because the sound subset
cannot rule out an extension homonym.

**`call.too-few-arguments`** and **`call.unknown-named-argument`** — the
arguments do not bind. Both need a uniquely resolved target.

```php
<?php

declare(strict_types=1);

function schedule(string $job, int $delay): void {}

schedule("reindex");
schedule(job: "reindex", timeout: 30);
```

```
$ steins check src/Arity.php
src/Arity.php:7:1: error[call.too-few-arguments]: too few arguments to schedule(): 1 passed, 2 required — provable ArgumentCountError
src/Arity.php:8:1: error[call.unknown-named-argument]: unknown named argument $timeout to schedule() — no parameter $timeout, provable Error
```

**`call.too-many-arguments`** is registered and not yet emitted. Extra
arguments to a userland function are silently ignored by PHP, so they are
never a finding; only an *internal* non-variadic target fatals, and that arm
waits on the reflection slice. The id is nameable today in `@steins-ignore`
and in profiles, and it produces nothing.

### `class.*` — a class name that resolves to nothing

One id, proof layer, default surface. It fires wherever a missing class-like
actually breaks the program: `new`, a static call, a class-constant or
static-property fetch (fatal `Error`); `extends`, `implements` and `use
<Trait>` (fatal at class load); `catch (X $e)` (the handler is silently
dead); and a parameter, return or property native type declaration
(`TypeError` on the first typed use — nullable, union and intersection
declarations report once per named arm). It stays silent where PHP does not
break: `instanceof` evaluates `false`, `X::class` is a plain string,
`self`/`static`/`parent` and the built-in type keywords are not class names,
and a docblock is a comment. Like the absence ids above, it needs the
sidecar.

```php
<?php

declare(strict_types=1);

namespace App;

$client = new QueueClient();
```

```
$ steins check src/Queue.php
src/Queue.php:7:15: error[class.undefined]: reference to undefined class App\QueueClient — not defined in the project, not on PHP 8.5.8 (70 extensions)
```

### `offset.*` — array reads that do not land

Four ids, split across two layers and three profile rungs. This is the one
family where reading the layer matters, because two ids prove things about
*values* and two prove things about *declarations* (ADR-0062).

**`offset.missing`** and **`offset.on-unsupported`** are proof layer, on the
default surface. Steins folded the container itself, so it knows the keys.

```php
<?php

declare(strict_types=1);

function host(): string
{
    $config = ['host' => 'localhost', 'port' => 5432];

    return $config['hostname'];
}

function port(): int
{
    $port = 5432;

    return $port['host'];
}
```

```
$ steins check src/Offsets.php
src/Offsets.php:9:12: error[offset.missing]: offset 'hostname' provably missing — $config is ['host' => 'localhost', 'port' => 5432] on this path; reads null with "Undefined array key "hostname""
src/Offsets.php:16:12: error[offset.on-unsupported]: offset read on $port — provably int; reads null with "Trying to access array offset on int"
```

**`offset.undeclared`** and **`offset.maybe-missing`** are contract layer.
The evidence is your docblock rather than a folded value, so the claim is
conditional on the declaration being true — which is exactly why they are
not proof-layer findings. `offset.undeclared` reaches the `contracts` rung;
`offset.maybe-missing` reaches only `strict`.

```php
<?php

declare(strict_types=1);

/** @param array{host: string, port?: int} $dsn */
function scheme(array $dsn): string
{
    return $dsn['scheme'];
}

/** @param array{host: string, port?: int} $dsn */
function port(array $dsn): int
{
    return $dsn['port'];
}
```

```
$ steins check --profile contracts src/Shape.php
src/Shape.php:8:12: error[offset.undeclared]: offset 'scheme' is outside the declared shape — $dsn is non-empty-array{host: string, port?: int}, which cannot carry the key; reads null with "Undefined array key "scheme""
```

```
$ steins check --profile strict src/Shape.php
src/Shape.php:8:12: error[offset.undeclared]: offset 'scheme' is outside the declared shape — $dsn is non-empty-array{host: string, port?: int}, which cannot carry the key; reads null with "Undefined array key "scheme""
src/Shape.php:14:12: error[offset.maybe-missing]: offset 'port' may be missing — $dsn is non-empty-array{host: string, port?: int}, which declares the key optional, and no guard on this path discharges it; reads null with "Undefined array key "port""
```

A guard discharges `offset.maybe-missing`. Wrap the read in
`isset($dsn['port'])` or `array_key_exists('port', $dsn)`, or put it behind
`??`, and the finding goes away on that path. `strict` is the stage that
asks you to prove presence instead of assuming it.

### `readonly.*` — a second write to a readonly property

One id, proof layer, default surface. Two proven writes on one path are a
guaranteed `Error`, including inside the constructor that made the first.

```php
<?php

declare(strict_types=1);

final class Order
{
    public function __construct(public readonly int $id)
    {
        $this->id = 7;
    }
}
```

```
$ steins check src/Readonly.php
src/Readonly.php:9:9: error[readonly.reassigned]: Cannot modify readonly property Order::$id — proven Error
```

### `phpdoc.*` — declared contracts you wrote in a docblock

Five ids, contract layer, `contracts` rung. PHP does not enforce PHPDoc at
runtime, so nothing here breaks your program. What breaks is the promise the
docblock makes to every reader and every tool downstream.

The acceptance relation is stricter than the runtime one. `"60"` satisfies a
native `int` parameter under coercive mode and never satisfies a
`@param int` — the docblock says the value *is* an int, and a numeric string
is not (ADR-0030,
[contract-types.md](../type-specification/contract-types.md)).

```php
<?php

declare(strict_types=1);

final class Session
{
    /** @var int */
    public $ttl = 60;

    /** @return int */
    public function remaining()
    {
        return "30";
    }
}

/** @param int $seconds */
function extend($seconds): void {}

$session = new Session();
$session->ttl = "60";
extend("60");
```

```
$ steins check --profile contracts src/Contract.php
src/Contract.php:13:16: error[phpdoc.return-mismatch]: return value "30" violates declared @return int of Session::remaining() — declared contract violation
src/Contract.php:21:1: error[phpdoc.property-mismatch]: value "60" assigned to property Session::$ttl violates declared @var int — declared contract violation
src/Contract.php:22:8: error[phpdoc.param-mismatch]: argument "60" to extend() violates declared @param int $seconds — declared contract violation
```

**`phpdoc.undefined-method`** is the family's fourth id and a different
shape: a method call whose receiver type comes from a `@param` rather than
from a folded value. Absence is proven under descendant closure, so an open
class with a subclass that defines the method stays silent.

```php
<?php

declare(strict_types=1);

final class Clock
{
    public function now(): int
    {
        return 0;
    }
}

/** @param Clock $clock */
function stamp($clock): int
{
    return $clock->currentTime();
}
```

```
$ steins check --profile contracts src/Receiver.php
src/Receiver.php:16:12: error[phpdoc.undefined-method]: call to undefined method Clock::currentTime() — declared receiver $clock narrowed to {Clock}, hierarchy and descendants fully enumerated, no __call
```

Write the same declaration natively and the finding changes id. A native
`Clock $clock` parameter is enforced by PHP itself — the call either passes a
`Clock` or has already thrown a `TypeError` — so the absence is proven, not
merely asserted, and the finding is `call.undefined-method` on the **default**
surface:

```php
function stamp(Clock $clock): int
{
    return $clock->currentTime();
}
```

```
$ steins check src/Receiver.php
src/Receiver.php:16:12: error[call.undefined-method]: call to undefined method Clock::currentTime() — declared receiver $clock narrowed to {Clock}, hierarchy and descendants fully enumerated, no __call, no @method/@property/@mixin
```

One `@param`-derived arm anywhere in the receiver's type is enough to put the
finding back on `phpdoc.undefined-method` and back behind `--profile contracts`:
the id follows the weakest premise the claim rests on. The evidence wording is
the same either way, so `declared receiver $x` in a `call.undefined-method`
message is how you tell this lane's findings from the exact-receiver ones.

**`phpdoc.maybe-undefined`** is the fifth, and the one that is about a
*binding* rather than a value. In a top-level script, `/** @var \DateTime|unset
$x */` says `$x` is either a `\DateTime` or **not defined at all** — the
included-partial idiom, where the file is handed its variables by whatever
included it. Steins reports nothing about presence in a top-level script
otherwise, and deliberately: an included file inherits the includer's symbol
table, so nothing in the text can claim a name is absent. The `unset` member is
you saying it, so it is you who lifts that silence, for that name only.

```php
<?php

/** @var \DateTime|unset $date */
echo $date->format('Y-m-d');

/** @var \DateTime|unset $other */
if (isset($other)) {
    echo $other->format('Y-m-d');
}
```

```
$ steins check --profile contracts view.php
view.php:4:6: error[phpdoc.maybe-undefined]: $date is declared \DateTime|unset and may be undefined at this read — guard it with isset($date) or give it a default
```

Everything that makes the read safe discharges it, from the point it appears:
`isset($x)` on its true branch, `!isset($x)` or `empty($x)` on their false
branches (an early `return` included), `$x ?? $default`, `$x ??= $default`, an
assignment, and the defaulting idiom `if (!isset($x)) { $x = …; }`. A guard
through a chain reaches its root, so `if (!isset($x['k'])) { return; }` guards
`$x`. Inside the guard the type is plain `\DateTime` — the `unset` member
carries no value — so member resolution is unchanged, and the guard is never
reported as redundant.

An `include`, `require`, `extract`, `compact`, `get_defined_vars`, `eval` or
`$$name` **ends** the claim rather than blanking the file: reads before it are
still judged, reads after it are not, because from there the symbol table is no
longer readable from the text. Only a top-level inline `@var` means this today;
in a function, `@param T|unset` and friends carry no semantics yet.

### `throw.*` — `@throws` envelopes

Two ids, contract layer. An unannotated function is never envelope-checked;
writing `@throws` is what opts a declaration in (ADR-0040).

**`throw.undeclared`** is the one id with a *facet*: `origin`, either
`direct` (the exception is thrown in the annotated body itself) or
`propagated` (it escapes through a call). The `throws-direct` profile
surfaces only the direct ones — the high-signal subset, where the docblock
is wrong about the method you are reading.

```php
<?php

declare(strict_types=1);

final class Importer
{
    /** @throws LogicException */
    public function run(): void
    {
        throw new RuntimeException('disk full');
    }

    /** @throws LogicException */
    public function runAll(): void
    {
        $this->run();
    }
}
```

```
$ steins check --profile throws-direct src/Throws.php
src/Throws.php:10:9: error[throw.undeclared]: RuntimeException can escape Importer::run() but is not declared (@throws LogicException) — proven escape
```

```
$ steins check --profile contracts src/Throws.php
src/Throws.php:10:9: error[throw.undeclared]: RuntimeException can escape Importer::run() but is not declared (@throws LogicException) — proven escape
src/Throws.php:10:9: error[throw.undeclared]: RuntimeException can escape Importer::runAll() but is not declared (@throws LogicException) — proven escape
```

Both findings point at the same `throw` statement, and the escaping function
named in the message is what distinguishes them. `contracts` added the
propagated one. Only proven escapes report — a `Maybe` escape, or a class
whose hierarchy Steins cannot fully resolve, stays silent.

**`throw.liskov-widened`** fires when an override declares a checked
exception the abstraction does not, and only when both sides declare
`@throws`.

```php
<?php

declare(strict_types=1);

abstract class Job
{
    /** @throws LogicException */
    abstract public function run(): void;
}

final class ImportJob extends Job
{
    /** @throws RuntimeException */
    public function run(): void {}
}
```

```
$ steins check --profile contracts src/Liskov.php
src/Liskov.php:14:21: error[throw.liskov-widened]: RuntimeException is declared thrown by ImportJob::run() but Job::run() (its abstraction) declares only @throws LogicException — Liskov widening
```

### `effect.*` — effect envelopes

Four ids in two layers. Effects are the second dimension Steins infers, and
the handbook's [effects chapter](../handbook/04-effects.md) is the tour.

**`effect.envelope-exceeded`** and **`effect.liskov-widened`** are contract
layer, `contracts` rung. The first judges a body against its own
`#[\Steins\Pure]` or `#[\Steins\Effect(...)]` declaration; the second judges
an override against the abstraction's declaration. Implementations may be
purer, never less pure.

```php
<?php

declare(strict_types=1);

abstract class Formatter
{
    #[\Steins\Pure]
    abstract public function format(string $line): string;
}

final class EchoFormatter extends Formatter
{
    public function format(string $line): string
    {
        echo $line;

        return $line;
    }
}

#[\Steins\Pure]
function slugify(string $line): string
{
    echo $line;

    return $line;
}
```

```
$ steins check --profile contracts src/Effects.php
src/Effects.php:13:21: error[effect.liskov-widened]: EchoFormatter::format() has proven effect output but Formatter::format() (its abstraction) is declared #[\Steins\Pure] — Liskov effect widening
src/Effects.php:24:5: error[effect.envelope-exceeded]: echo has effect output, but slugify() is declared #[\Steins\Pure]
```

**`effect.unknown-label`** is mechanics, and prints in every profile
including a bare `check`. A typo'd label silently disables the envelope that
contains it, which is the rot the mechanics layer exists to catch.

```php
<?php

declare(strict_types=1);

#[\Steins\Effect('io.netwrok')]
function fetch(string $url): string
{
    return $url;
}
```

```
$ steins check src/Label.php
src/Label.php:5:3: error[effect.unknown-label]: unknown effect label 'io.netwrok' in #[\Steins\Effect] on fetch()
```

**`effect.interop-unknown-label`** is the same mistake made in one of
PHPStan's purity tags instead — and it is contract layer, `contracts` rung,
because a docblock is not Steins' own syntax. The reading rule is the one
[the interop
spec](../type-specification/phpdoc-effects-interop.md#unknown-labels)
describes: a label Steins does not recognize makes the **whole tag**
unspecified, so `/** @phpstan-impure io.netw */` quietly bounds nothing. This
id is what stops that from being silent.

It fires only where something says the token was *meant* as a label: it is
close to a real one, another member of the same list is a real one, it has two
or more dot segments, or it is a spelling Steins retired. A one-word note —
`/** @phpstan-impure database */`, which current PHPStan discards and which
therefore appears in real docblocks — matches none of those and never reports,
on any profile. That silence is a promise, not an oversight.

The migration case is the one most projects meet first. `output` became
`io.output.buffer` / `io.output.header` / `io.output` in v0.1.x, and `output` →
`io.output` is three edits — too far for a "did you mean", which is why the
replacement is written out instead:

```php
<?php

declare(strict_types=1);

final class Renderer
{
    /** @phpstan-impure output */
    public function render(string $line): void
    {
        echo $line;
    }

    /** @phpstan-impure io.netw */
    public function fetch(string $url): string
    {
        return file_get_contents($url);
    }
}
```

```
$ steins check --profile contracts src/Renderer.php
src/Renderer.php:8:21: error[effect.interop-unknown-label]: unknown effect label 'output' in @phpstan-impure on Renderer::render() — the whole tag reads as unspecified and bounds nothing; 'output' was retired, so write io.output.buffer for echo-shaped code, io.output.header for header()/setcookie(), or the umbrella io.output
src/Renderer.php:14:21: error[effect.interop-unknown-label]: unknown effect label 'io.netw' in @phpstan-impure on Renderer::fetch() — the whole tag reads as unspecified and bounds nothing; did you mean 'io.net'?
```

A bare `steins check` prints neither: enabling envelope enforcement is what
turns on the check that keeps enforcement honest. Being contract layer, it is
also suppressable — `@steins-ignore effect.interop-unknown-label`, or a
baseline entry — which is what a codebase halfway through a rename needs.

### `untyped.*` — the type declarations you have not written yet

Six ids, contract layer. This family is the odd one out: every other family
reports a claim that *disagrees* with your code, and this one reports a claim
your code never makes. It answers one question — how much untyped surface is
left, and where — which is the question a modernization project actually has.

Five of the six are on `contracts`. The sixth is on `pedantic` and nothing
else; that split is the family's whole design and is explained below.

A type written **anywhere** counts. A native declaration types the subject, a
docblock claim types it, and a claim that turns out to be *wrong* still types
it — a wrong claim is `phpdoc.*`'s finding, never this family's. Steins
reads `@param` / `@return` / `@var` and their `@phpstan-` and `@psalm-`
prefixed spellings alike.

```php
<?php

declare(strict_types=1);

final class Registry
{
    const DEFAULTS = ['retries' => 3];

    public $cache;

    /** @var array */
    public array $entries = [];

    public function put($key, array $rows)
    {
        $this->entries[$key] = $rows;
    }
}
```

```
$ steins check --profile contracts src/Untyped.php
src/Untyped.php:9:5: error[untyped.property]: property Registry::$cache has no type — no native type and no `@var`
src/Untyped.php:12:5: error[untyped.iterable-value]: property Registry::$entries is an iterable with no value type — write `array<T>`, `T[]`, `list<T>` or an array shape
src/Untyped.php:14:21: error[untyped.return]: Registry::put() has no return type — no native return type and no `@return`
src/Untyped.php:14:25: error[untyped.parameter]: parameter $key of Registry::put() has no type — no native type and no `@param`
src/Untyped.php:14:31: error[untyped.iterable-value]: parameter $rows of Registry::put() is an iterable with no value type — write `array<T>`, `T[]`, `list<T>` or an array shape
```

**`untyped.parameter`** — a parameter with no native type and no `@param`.
Variadic (`...$args`) and by-ref (`&$x`) spellings still name the parameter.
A promoted constructor parameter reports here and *only* here: one
declaration, one finding.

**`untyped.return`** — no native return type and no `@return`.
`__construct` and `__destruct` are excluded by construction, since PHP
forbids a return type on either — their silence is a language rule, not
information withheld.

**`untyped.property`** — no native type and no `@var` on the declaration.
Each item of `public $a, $b;` is its own subject, and one `@var` above the
declaration types them both.

**`untyped.iterable-value`** — a native `array` or `iterable` whose docblock
never states the *value* type. `array` is a real type, so the plain parameter
and property arms stay quiet; this id is the one that asks what is inside.
`array<T>`, `T[]`, `list<T>`, `iterable<T>` and an array shape all answer it.

**`untyped.generics`** — a docblock type naming a class that declares
`@template` parameters, written without type arguments.

```php
<?php

declare(strict_types=1);

/** @template T */
final class Collection {}

final class Report
{
    /** @param Collection $rows */
    public function render(Collection $rows): void {}
}
```

```
$ steins check --profile contracts src/Generics.php
src/Generics.php:11:28: error[untyped.generics]: parameter $rows of Report::render() names the generic class Collection without type arguments — it declares `@template T`
```

**`untyped.class-constant`** — a class constant with no native (PHP 8.3)
constant type and no `@var`. **This one is on `pedantic`, not `contracts`.**
A constant is inherently static: its initializer is a constant expression, so
`const DEFAULTS = ['retries' => 3];` has exactly the type it would have with
the declaration written out. Nothing is withheld, which is not true of any
other arm — a parameter, property or return with no type is `mixed`, and the
analyzer has to guess. Inheritance can still overwrite a constant with a
differently shaped value, and Steins takes that risk knowingly rather than
asking you for a declaration that buys it nothing.

So Steins does not ask. A team that wants every constant annotated does,
and says so by name:

```
$ steins check --profile pedantic src/Untyped.php
src/Untyped.php:7:11: error[untyped.class-constant]: class constant Registry::DEFAULTS has no type — no native type and no `@var`
src/Untyped.php:9:5: error[untyped.property]: property Registry::$cache has no type — no native type and no `@var`
...
```

Enum cases are never subjects — a case's type *is* its enum. An enum's
ordinary constants still are.

This family is what the baseline ratchet was built for. A repo with years of
untyped surface produces a large first run by design; freeze it with
`--set-baseline` and only newly untyped declarations fail CI. See
[chapter 5](05-profiles-and-baseline.md).

### `suppress.*` — the ignore channel keeping itself honest

Two ids, mechanics layer, every profile, fail level, exempt from every
suppression channel. You cannot ignore them, baseline them, or turn them off
in a profile — a dead suppression that never bites is a suppression nobody
ever removes.

```php
<?php

declare(strict_types=1);

function charge(int $cents): void {}

// @steins-ignore call.on-null
charge(1200);

// @steins-ignore type.argument-mismach
charge("1200");
```

```
$ steins check src/Ignore.php
src/Ignore.php:7:1: error[suppress.unmatched]: @steins-ignore of call.on-null matches no diagnostic on line 8
src/Ignore.php:10:1: error[suppress.unknown-id]: @steins-ignore names unknown diagnostic id 'type.argument-mismach'
src/Ignore.php:11:8: error[type.argument-mismatch]: argument "1200" to charge() cannot become int $cents — proven TypeError (strict mode)
```

Three lines from two mistakes. The first ignore names a real id that matches
nothing on its target line. The second misspells `type.argument-mismatch`,
so the ignore is rejected *and* the finding it meant to suppress prints
underneath. Ignore syntax and placement rules are in
[chapter 5](05-profiles-and-baseline.md).

### `debug.*` — you asked, Steins answered

Four ids, debug layer, on every surface. These report what the analyzer
inferred at a point, which makes them the fastest way to find out why some
*other* finding did or did not fire.

```php
<?php

declare(strict_types=1);

/** @param non-empty-string $method */
function route($method): void
{
    \PHPStan\dumpPhpDocType($method);
}

$verb = 'POST';
\PHPStan\dumpType($verb);

$limit = 25;
var_dump($limit);
```

```
$ steins check src/Dump.php
src/Dump.php:8:29: error[debug.phpdoc-type]: dumped phpdoc type: non-empty-string (asserted)
src/Dump.php:12:19: error[debug.type]: dumped type: 'POST'
src/Dump.php:15:10: warning[debug.var-dump]: dumped type: 25
```

`debug.type` renders the walk's best knowledge of a value — here the proven
literal `'POST'`, not the type `string`. `debug.phpdoc-type` renders the
declared side instead, the contract arms as narrowed by guards.
`debug.var-dump` reports one line per argument of any default-on `var_dump()`.

The fourth id is the committable spelling of the `debug.type` question
(ADR-0074): a `/** @psalm-trace $x */` (or `@phpstan-trace`) docblock above
a statement reports the same rendering, answered against that statement's
*exit* facts — the tag applies to the next statement and reports what it
leaves behind, so a trace above an assignment prints what the variable
became. A comma list (`@psalm-trace $a, $b`) reports each named variable in
source order.

```php
<?php

/** @psalm-trace $verb */
$verb = 'PUT';
```

```
$ steins check src/Trace.php
src/Trace.php:3:5: warning[debug.trace]: traced type of $verb: 'PUT'
```

The levels differ on purpose. The explicit pair is **fail** and reds your
build, because `PHPStan\dumpType` is not a real PHP function and a committed
call is a guaranteed fatal. `var_dump()` is legal working PHP, so
`debug.var-dump` is **warn** and exit-neutral by construction — no channel
can promote it to fail. Silence it with `disable = ["debug.var-dump"]` in a
named profile. `debug.trace` is **warn** and exit-neutral for the mirror
reason — a docblock is runtime-inert and legal to commit — but it has no
disable switch: an annotation is always an authored question, and the remedy
is deleting the comment.

All four are exempt from `@steins-ignore` — an ignore naming `debug.type`
reports `suppress.unmatched`. ADR-0053 exempts them from the baseline as
well: `--set-baseline` never writes a debug entry, and a leftover one (from
before this exemption, or a hand-edit) never suppresses a dump either — it
resurfaces as a stale baseline entry instead. The remedy for an unwanted
dump is deleting the call (ADR-0053) — and for the explicit pair, `check
--fix` does exactly that: `debug.type` and `debug.phpdoc-type` carry the
statement deletion as a fix payload (ADR-0010), applied under a
post-check gate. See the [`--fix` section of the CLI
reference](02-cli-reference.md#--fix). `debug.var-dump` carries no fix —
deleting working PHP is your judgment call.

## What to do with a finding

1. **Fix it.** For a proof finding this is the only real option. It is a
   runtime break, and Steins was held to a zero-false-positive bar before it
   was allowed to say so.
2. **Discharge it.** Some findings are answered by code rather than by
   configuration: a guard discharges `offset.maybe-missing`, a `catch` dams
   a `throw.undeclared`, a widened envelope admits an effect.
3. **Suppress the one site**, with an `@steins-ignore` naming the id.
4. **Freeze today's debt** with a baseline when you raise a profile over an
   existing codebase, so only new findings fail CI.

Options 3 and 4, the five named profiles, and the ratchet workflow that walks
a repo from `default` to `strict` without a first run that buries you are
all in [profiles, baseline, and suppression](05-profiles-and-baseline.md).
The binding rules behind everything on this page are in
[diagnostic-policy.md](../type-specification/diagnostic-policy.md).
