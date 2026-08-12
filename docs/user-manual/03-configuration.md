# Configuration

`steins.toml` is the one config file Steins reads. Every key in it is
optional — an absent file, or an absent key inside a present file, leaves
the built-in default in force (ADR-0023). This chapter is the key-by-key
reference: what each key means, what it defaults to, and how it interacts
with the equivalent command-line flag. For the *subcommands* and their
flags, see [the CLI reference](02-cli-reference.md); for what a profile
stage actually surfaces, see
[profiles, baseline, and suppression](05-profiles-and-baseline.md).

## A complete example

Every section below appears here at least once, each with a one-line
comment. This file parses clean — `steins doctor` reports it as found and
resolved, `steins check` reports no config warnings:

```toml
# steins.toml lives at the repo root; every key below is optional, and an
# absent file leaves every built-in default in force.

[check]
# The default profile for a bare `steins check` — this repo's declared
# strictness stage. `--profile <name>` on the command line overrides this.
profile = "migration"

# A user-defined profile: `extends` a built-in or another user profile,
# then refines it with ADR-0022 prefix id-arrays.
[profile.migration]
extends = "contracts"
warn    = ["throw.*"]

[runtime]
# What a proven E_WARNING does at runtime (ADR-0049 §7). "abort" (the
# default) treats it as a proven break; "null" declares the app tolerates
# it, and the corresponding proof-layer findings stay silent.
warning-handler = "abort"

# What the runtime does with the `final` keyword. "enforced" (the default)
# is PHP's own rule; "stripped" declares a loader that rewrites `final`
# away, the way dg/bypass-finals does under a test harness.
final-keyword = "enforced"

[plugins]
# Explicit plugin allowlist, by Composer package name (ADR-0039/0068).
# Replaces installed.json discovery outright; allow = [] loads nothing.
allow = []

[effects]
# Effect labels the envelope judgment discharges project-wide (ADR-0084).
# The labels stay in the catalog and in `annotate`; only verdicts change.
tolerated = ["telemetry"]

[effects.attribution]
# What a symbol's effects are *for*. Fact, not policy: inert until some
# label it introduces appears in `tolerated` above.
"Monolog\\Logger" = ["telemetry"]

[transform.vouch]
# Dynamic-code sites a human has reviewed and vouched for (ADR-0046 §2).
# Read only by `steins transform`.
sites = []

[transform.partitions]
# Path sets that may reference any partition without triggering a
# cross-partition finding (ADR-0047 §1). Read only by `steins transform`.
observers = ["tests/**"]

[transform.partitions.sets]
core = ["src/**"]
```

Running `check` against a project with this file and one undeclared
`throw` shows the `migration` profile in force — `throw.*` demoted to
`warning`, exit `0`:

```
$ steins doctor .
Config + active surface
  steins.toml: found
  active profile: `migration` (from [check] profile)
  surface: layers [contract, mechanics, proof], 25 checked id(s)

$ steins check src/
src/App.php:11:9: warning[throw.undeclared]: RuntimeException can escape Svc::run() but is not declared (@throws LogicException) — proven escape
$ echo $?
0
```

## Discovery

`check` and `doctor` read exactly one path: `steins.toml`, resolved
relative to the **current working directory** — literally, no walk-up of
parent directories and no search for a project root. Run either command
from a subdirectory and a `steins.toml` sitting one level up is invisible;
it takes the built-in defaults instead of erroring — silently, because a
missing file is a legitimate zero-config state (ADR-0020). Neither
subcommand accepts a `--config` flag — there is no way to point either one
at a file elsewhere.

`transform` accepts `--config <path>` to read from somewhere other than
`./steins.toml`; omitting the flag falls back to the same
current-working-directory `./steins.toml` the other subcommands use.
`annotate` and `effect-diff` read only the `[plugins]` table (below), and
do so leniently — a malformed file is treated the same as no file, with no
warning printed.

> **If you know PHPStan or Psalm:** both tools search upward for
> `phpstan.neon`/`psalm.xml` from the analyzed path. Steins does not — put
> `steins.toml` where you run the command from, normally the repo root
> that also holds `composer.json`, though nothing enforces that the two
> coincide.

**Parsing is strict where a wrong default is dangerous, lenient
elsewhere.** `check` and `doctor` parse the whole file up front, before any
analysis runs, and a malformed file is a **hard config error**: exit `2`
for `check`, and for `doctor` a reported *configuration contradiction*
(exit `1`, the report still renders on the built-in `default` surface).
This includes an unrecognized key inside `[runtime]` or `[plugins]` — both
reject unknown fields on purpose, because a silently-ignored typo there
(`warning-hadler`, say) would leave the safe default in force while you
believed you had overridden it. `transform`'s `[transform.vouch]` and
`[transform.partitions]` readers are lenient instead: a parse error there
prints a warning and the run proceeds with no vouches or no partitions,
because a config typo should not stop a transform from running.

An unrecognized *top-level table*, and an unrecognized key inside `[check]`
or `[profile.<name>]`, are silently ignored everywhere — they carry no
`deny_unknown_fields` guard. A misspelled `[chekc]` produces no warning and
no error; it never takes effect. `steins doctor` is the fast way to
confirm a section landed: its "Config + active surface" section names the
active profile and its provenance.

## Key-by-key reference

### `[check]`

Repo defaults for `steins check`.

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `profile` | string | unset (built-in `default`) | The profile name to run when `--profile` is not given on the command line. |

### `[profile.<name>]`

Zero or more named tables, each defining one user profile (ADR-0050 §5).
The table name is the profile name; `default`, `contracts`,
`throws-direct`, `strict` and `pedantic` are built in and cannot be
redefined, and
`boundary` is reserved (see below) — defining or selecting either is a
config error.

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `extends` | string | `"default"` | The base profile this one refines: a built-in name or another `[profile.<name>]`. `extends` chains that cycle are a config error. |
| `enable` | array of string | `[]` | ADR-0022 prefix id-patterns (`"throw.*"`, or a full id) forced onto the surface beyond what `extends` already admits. |
| `disable` | array of string | `[]` | Prefix id-patterns removed from the surface. Mechanics-layer ids (`suppress.*`, `effect.unknown-label`) ignore this — they print on every profile, unconditionally. |
| `warn` | array of string | `[]` | Prefix id-patterns demoted from `fail` to `warn`: still printed, but the run exits `0` on those findings alone. |

A pattern that names no registered id (`not.an.id`) is a config error, as
is a facet-shaped pattern (`throw.undeclared@direct`) — v1 reaches the
`origin` facet only through the built-in `throws-direct` profile, not
through a user one.

### `[runtime]`

Boot-truth facts the checker cannot observe from source (ADR-0037 §2).
This section rejects unrecognized keys outright.

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `warning-handler` | `"abort"` \| `"null"` | `"abort"` | What a proven `E_WARNING` does at runtime. `"abort"` treats it as a proven break, so the corresponding proof-layer findings fire. `"null"` declares the app tolerates the warning; those findings go silent. An unrecognized *value* (not an unrecognized key) warns and falls back to `"abort"` rather than erroring. |
| `final-keyword` | `"enforced"` \| `"stripped"` | `"enforced"` | What the runtime does with the `final` keyword. `"enforced"` is PHP's own rule: a `final` class admits no subtype, so an intersection carrying a final class arm is uninhabited. `"stripped"` declares a loader that rewrites the keyword away before the class is compiled — `dg/bypass-finals` installs a stream wrapper that does exactly this — so `FinalClass&MockObject`, the type a mock of a final class actually has, stays inhabited. An unrecognized *value* warns and falls back to `"enforced"`. |

There is no CLI flag for either key — both are config-only.

`steins doctor` prints both postures, with their value and whether it came
from the file or from the default, in the Config section.

### What `final-keyword = "stripped"` does not change

The posture is deliberately narrow. It withdraws exactly one emptiness
proof and nothing else:

- **`readonly` is untouched.** `dg/bypass-finals` strips `readonly` only
  when explicitly asked (`enable(bypassReadOnly: true)`); the two are
  separate knobs in the library, so they stay separate here.
  `readonly.reassigned` fires identically under both postures.
- **The `final` diagnostics are untouched.** `class.extends-final` and
  `override.final` still fire: source that *spells* `extends FinalClass`
  is broken under a plain runtime whatever a test harness rewrites at load
  time.
- **Nothing is detected.** Steins never infers a final-stripping runtime
  from a loaded `uopz`/`runkit7` or from your dependency graph. The
  posture is declared or it is absent.
- **It is project-wide.** The library's own `denyPaths([...])` scoping has
  no equivalent here yet; a path-scoped posture would key on ADR-0047
  regions, which the check lane does not carry.

### `[plugins]`

The explicit plugin listing (ADR-0039 discovery, ADR-0068 §2 ownership).
This section rejects unrecognized keys outright.

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `allow` | array of string | unset (installed.json discovery governs) | Composer package names to load as plugins, **replacing** `installed.json` discovery outright. `allow = []` is meaningful — it loads no plugins — distinct from omitting the key, which leaves discovery in charge. Listing a plugin also vouches for its identity for label-registration purposes. |

Every subcommand reads `[plugins]`, and does so leniently: a malformed
`steins.toml` is treated as no plugin config (discovery governs) rather
than aborting the command, even for `check` and `doctor`, which are
otherwise strict about the rest of the file.

### `[effects]`

Effect labels this project has decided not to be told about (ADR-0084).
The table answers the one shape envelope checking cannot: a system-wide
logger transitively reaches the clock and the filesystem, so under
whole-program propagation every declaration that can touch logging loses
purity and earns an `effect.envelope-exceeded`. Those findings are honest
and unactionable at once.

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `tolerated` | array of string | `[]` | Labels the envelope judgment **discharges** before comparing proven effects against a bound. Subsumption applies: tolerating `nondet` tolerates `nondet.time`. |
| `attribution` | table of string → array of string (as `[effects.attribution]`) | `{}` | Symbol → labels marking what that symbol's effects are *for*. Keys name a class (every method), a `Class::method`, or a global function. A label introduced here counts as project-declared for validation. |

**`tolerated` is the policy; `[effects.attribution]` is fact.** Attribution
alone changes no verdict — it marks a logging facade as being *for*
telemetry and stops there. Tolerance alone is blunt — `tolerated =
["nondet.time"]` silences the log timestamp and the business logic that
branches on today's date in the same stroke. Written together they compose
into the precision you want: the effects that arrived through the
attributed facade discharge, and the same `time()` call reached any other
way keeps its finding.

Three things the table deliberately does not do. **It never edits the
catalog:** `time()` stays `nondet.time`, propagation is untouched, and
`annotate` still shows every label. **It never travels:** the discharge
applies at judgment — `effect.envelope-exceeded` on both the attribute and
the interop-envelope stratum, and the purity oracle behind `pure-callable`
— but never at a spelling-producing site, so `steins transform
effects-envelope` keeps judging by the undischarged effects and will not
write a `@phpstan-pure` a function earned only under your policy. A
docblock outlives the config file. **It is not a profile field:** profiles
select which findings surface, and this changes which findings exist, so it
sits at the top level and profile switching stays free of it.

Labels in either key are validated against the known vocabulary —
builtin, plugin-registered, and attribution-declared — with the same
nearest-suggestion treatment the rest of the label surface has. An
attribution key naming a symbol the run never resolves is a notice, not an
error: vendor code comes and goes. The failure direction is toward more
findings, never fewer — a table that does not take effect leaves the
findings exactly where they were.

`steins check --no-tolerated-effects` runs the judgment with an empty
tolerance set. That is the audit switch: every discharged finding comes
back, so "what is my policy currently hiding" is a flag away rather than a
config edit. `annotate` marks a label wholly discharged at a unit with a
tilde — `effects: {~nondet.time, io.db}` — and `--format json` grows a
`tolerated` array beside `labels`; `labels` itself is unchanged, and a
label only some of whose arrivals discharge stays unmarked.

### Choosing what to tolerate

The mechanism takes any label. The guidance is about which ones are safe
to hand it:

- **Tolerate semantic labels, not transport labels.** `telemetry` is a
  statement about purpose and can only reach what you attributed;
  `io.fs.write` is a statement about machinery and reaches everything.
- **Never attribute a PSR-14 `dispatch()`.** An event dispatcher looks
  like a logger and is not one: real frameworks routinely consume the
  returned event object, so the call is observable to the program and a
  listener's effects are the caller's business.
- **Keep `audit` separate from `telemetry`.** Compliance trails and debug
  logging share a shape and have opposite risk profiles — "safe to stop
  watching" versus "must always fire". Give audit logging its own label
  so a project can tolerate one without the other.

A worked shape — a project-local logging facade, and a class that mixes
both kinds of clock read:

```toml
[effects]
tolerated = ["telemetry"]

[effects.attribution]
"App\\Support\\Trace" = ["telemetry"]
```

```php
final class Trace
{
    public static function debug(string $line): void
    {
        error_log(date('H:i:s') . ' ' . $line);
    }
}

/** @phpstan-all-methods-pure */
final class Steam
{
    public function floatalize(string $value): float
    {
        Trace::debug(sprintf('floatalize("%s")', $value));
        return (float) str_replace(',', '.', $value);
    }

    public function isStale(DateTimeImmutable $seen): bool
    {
        return $seen->getTimestamp() < time() - 3600;
    }
}
```

`floatalize()` reaches `nondet.time` and `io` only through the log line,
every arrival attributed `telemetry`, so the class-level pure declaration
is no longer violated there. `isStale()` reads the clock as logic,
nothing attributed it, and its `effect.envelope-exceeded` is reported
exactly as before. The attribution is written once, against the facade,
and no call site is annotated. Builtins take attribution directly too —
`"error_log" = ["telemetry"]` stamps that builtin's own findings at every
call site, no wrapper needed.

One honesty note about framework logging. A Laravel `Log::` facade
resolves through `__callStatic`, an injected PSR-3 `LoggerInterface` is
dynamic dispatch, and direct Monolog bottoms out in internal
constructors the effect catalog has no rows for — none of those paths
contributes proven effects to a caller today, so there is nothing for
this table to discharge there yet. The policy grips effects the analyzer
can actually see arrive: project-local facades and builtins, which is
where envelope pollution actually manifests in a codebase that has it.

### `[transform.vouch]`

Dynamic-code sites vouched for by a human (ADR-0046 §2). Read only by
`steins transform`.

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `sites` | array of string | `[]` | `"file:line"` entries marking a dynamic-code site (`eval`, a non-literal `include`, a runtime-name `class_alias`) as reviewed. A malformed entry — no colon, or a non-numeric line — is skipped with a warning; it does not fail the run. |

### `[transform.partitions]`

Declared project regions for cross-partition precondition checking
(ADR-0047 §7). Read only by `steins transform`. With this section absent,
the whole project is one region and transform behavior is unchanged.

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `observers` | array of string | `[]` | Path globs (tests, dev-scripts) allowed to reference any partition without a cross-partition finding. |
| `sets` | table of string → array of string (as `[transform.partitions.sets]`) | `{}` | Partition name → path-glob list. Partitions must be pairwise disjoint; two sets whose globs can match the same path is a config error (exit `2`). |

## Designed, not yet shipped

ADR-0023 also specifies `[paths.sets]` (named path sets, referenced as
`@name`) and `[[policy]]` (scoped enable/disable rules matched by path set
and by semantic `where` matchers) as the third suppression channel,
alongside the baseline and inline `@steins-ignore`. Neither exists in the
binary yet — `[[policy]]` is tracked as issue #15 and ships as a no-op
pipeline stage. Writing either table into `steins.toml` today parses
without error and does precisely nothing, for the same reason a misspelled
`[check]` does nothing: neither is a field on any config struct the binary
deserializes, so both fall through the same silent top-level-table
tolerance described above. Do not rely on this working — it is a gap
that closes, not a feature.

## Precedence: CLI flag versus config key

Two keys have a CLI counterpart. `[check] profile` and `--profile <name>`
on `steins check` both select the active profile, and the flag wins.
Verified:

```
$ steins check src/                 # steins.toml sets [check] profile = "migration"
src/App.php:11:9: warning[throw.undeclared]: … — proven escape
$ echo $?
0
$ steins check --profile default src/
$ echo $?
0
```

The second run's `--profile default` overrides the config's
`profile = "migration"` outright — the surface it resolves is plain
`default` (proof + mechanics only), which is why the `throw.undeclared`
finding above disappears entirely rather than merely changing level.
Absent both a flag and a `[check] profile` key, the active profile is the
built-in `default`. `steins doctor` has no `--profile` flag of its own, so
its "active profile" line reports only `[check] profile` or
`built-in default` as the provenance — it names what the config declared,
not what a flag would have overridden.

`[effects] tolerated` is the second, and the flag goes one way only:
`--no-tolerated-effects` empties the tolerance set for that run, and
there is no flag that adds a label to it. The asymmetry is the point — a
tolerance is a reviewed decision that belongs in a diff, while removing
one for an audit run is exactly the thing you want to be able to do from
a shell.

Every other key is config-only: `[runtime] warning-handler`,
`[runtime] final-keyword`,
`[plugins] allow`, `[effects.attribution]`, `[transform.vouch]`, and
`[transform.partitions]` have no flag equivalent. `[profile.<name>]`
*definitions* are config-only too —
selecting one is `--profile <name>`, but there is no way to define a
profile's `enable`/`disable`/`warn` arrays from the command line. The
baseline file path is the mirror case: `--baseline <path>` is a flag with
no config-key equivalent; `steins.toml` carries no baseline setting at
all, so an unqualified `check --set-baseline` and a bare `doctor` both
fall back to the hardcoded conventional filename. See
[profiles, baseline, and suppression](05-profiles-and-baseline.md) for
that filename and the baseline workflow.

## What is deliberately not configurable

Three things `steins.toml` will never grow, on purpose:

- **No per-diagnostic on/off or demotion switch for a mechanics id.**
  `suppress.*` and `effect.unknown-label` print, and fail, on every profile
  regardless of `disable` or `warn`, because their whole job is to catch a
  suppression channel rotting — disabling or merely demoting the watchdog
  defeats it.
- **No ad-hoc `--enable id,id` flag.** Every surface a project runs under
  CI is a named profile in `steins.toml`, reviewable in a diff and stable
  across runs; an unnamed command-line surface is neither.
- **`boundary` is a reserved profile name** (ADR-0042). Selecting it with
  `--profile boundary` or `[check] profile = "boundary"`, or defining
  `[profile.boundary]`, is a config error until the boundary-profile
  design lands — verified:

  ```
  $ steins check --profile boundary src/
  steins: profile `boundary` is a reserved name (deferred to its ADR); it cannot be selected or extended yet
  $ echo $?
  2
  ```

The rationale for all three lives with the profile and surface semantics
they protect — see
[profiles, baseline, and suppression](05-profiles-and-baseline.md).

## Where to go next

- **Every flag, every subcommand:** [the CLI reference](02-cli-reference.md).
- **What a profile actually surfaces:**
  [profiles, baseline, and suppression](05-profiles-and-baseline.md) — the
  named stages, the ratchet workflow, exit-level semantics, and
  `@steins-ignore`.
- **Wiring this into CI:** [CI integration](06-ci.md).
