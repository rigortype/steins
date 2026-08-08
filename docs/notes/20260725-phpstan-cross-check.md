# PHPStan cross-check over the field survey — four recall gaps

PHPStan 2.2.5 (the current release) run over the same fourteen held-out
applications as the [adoption drill](20260724-adoption-drill-record.md), used as
an independent oracle against Steins at the T0 revision (`ReturnSummary` landed,
1394 tests). The harness lives outside this repo, in
`~/repo/php/steins-survey/.phpstan-survey/` (configs, per-app baselines, raw
JSON for both tools, and the scripts that produce them; `REPORT.md` there is the
long form, in Japanese).

This note records what the comparison found. Two things it did **not** find are
worth stating first: the default surface still carries **zero false positives**
over ~237k files, and both of the Steins findings that PHPStan can express at
the compared level were confirmed by PHPStan on the same line. The four items
below are recall gaps, not soundness breaks — except item 1, which is both.

## Method, and the trap in it

Level 2 uniformly (that is the PHPStan level whose checks overlap the Steins
default surface: undefined classes/methods/functions, argument counts, calls on
non-objects), `phpVersion` pinned per app from its composer constraint, tests
included so the path set matches what Steins analyses.

The first measurement was **discarded**. Registering `vendor/` through
`scanDirectories` does not resolve inherited members, so every test class
reported `assertSame()` as undefined. Minimal case:

```php
class XTest extends PHPUnit\Framework\TestCase {
    function t(): void { $this->assertSame(1, 1); }
}
```

`scanDirectories: vendor` → `method.notFound`; `bootstrapFiles:
vendor/autoload.php` → clean. Correcting it took the fleet from 145,068 to
50,679 errors. Loading each app's own bootstrap matters again on top of that:
passbolt_api drops 2,706 → 1,059 once `config/bootstrap.php` (which installs
CakePHP 5's global function shim) is loaded. **Most of PHPStan's volume is a
function of setup quality, not of code quality** — worth remembering whenever a
comparison against it is quoted.

Fleet totals after correction: **PHPStan 49,032** vs **Steins 8 default /
1,367 contracts**.

## 1. Symlinked duplicates silently drop declaration-dependent findings

`nextcloud-server/apps/user_ldap/tests/Integration/` contains **fourteen
provable `ArgumentCountError` sites** — stale test helpers such as
`new UserMapping(Server::get(IDBConnection::class))` against a six-parameter
constructor. PHPStan reports all of them. Steins reports **none** on a whole-tree
run, and **thirteen** when pointed at `apps/user_ldap` alone.

Bisecting the difference lands on `build/frontend/`, a directory containing **no
PHP files at all**. What it does contain is
`build/frontend/apps/user_ldap -> ../../../apps/user_ldap` and seventeen sibling
symlinks. `collect_php_files` follows directory symlinks and the collected list
is sorted and deduped **by path string**, so every class under `apps/` is
ingested twice under two different paths. Every class-like is then declared
twice, the existence guard concludes the hierarchy is not enumerable, and the
findings are dropped without a word.

Minimal reproduction (shipped as
`.phpstan-survey/repro-symlink-suppression.sh`):

```
src/a.php            class Mapper { __construct(3 required) } + new Mapper(1);
mirror/src -> ../src (directory symlink)

steins check src              → 1 × call.too-few-arguments
steins check src mirror       → nothing
```

Splitting by finding kind:

- **declaration-dependent** (arity, undefined-method, existence) — silently lost;
- **flow-derived** (`call.on-null`) — survives, but is reported **twice**, once
  per path.

ADR-0049's posture is *silence when the world is incomplete*. Here the world is
**duplicated**, and the same guard reads that as incomplete. Canonicalising
paths before dedup fixes both halves. This is the one item that is also a
soundness concern: an attacker-free, ordinary repository layout turns whole
finding categories off with no diagnostic.

## 2. Syntax errors pass silently

`pixelfed/app/Jobs/GroupsPipeline/DeleteCommentPipeline.php:49` contains
`if($this->status->)`. `php -l` rejects it; the commit that introduced it is
titled "Lint". PHPStan reports it and, because the autoloader parses the file
whenever a referencing file is analysed, **aborts the entire run** — pixelfed is
unanalysable until either that file or every file referencing it is excluded.

Steins says nothing. `SourceTree::parse` recovers, and `parse_errors()` has no
consumer anywhere outside `crates/steins-syntax/tests/smoke.rs`. Inference then
proceeds over the recovered tree and emits proof-grade findings from it. Two
consequences worth separating: the missing diagnostic (easy), and the question
of whether any finding derived from a recovered tree should be allowed to claim
proof (a posture question for an ADR).

## 3. Member checks only reach `new`-typed and static receivers

> **Corrected 2026-08-08 (issue #196).** The probe below measured a bare
> `steins check`, and read a *floor* as a reach boundary. The ADR-0049 §8
> declared-receiver lane already bound `C $o`; its only id sat on the contract
> layer, so nothing printed without `--profile contracts`. ADR-0049 A13 routes the
> lane by minimum stratum, and `viaParam` — a native declaration, `Verified` —
> now reports `call.undefined-method` on the **default** profile, matching
> PHPStan on this line. Also fixed in the same slice: a parameter copied into
> another variable (`$c = $o; $c->nope();`) carries its declared arms and
> reports. Still out of reach, and now for stated reasons rather than by
> omission: property and promoted-property receivers (ADR-0052 N5 — a property
> chain is a Barrier) and return-typed call receivers (`mk()->nope()` has no
> receiver representation in the trace at all). The paragraph after the probe
> stands only for those three shapes.

```php
final class C { public function ok(): void {} }
function viaParam(C $o): void { $o->nope(); }    // Steins silent, PHPStan reports
function viaNew(): void { (new C())->nope(); }  // both report
function viaStatic(): void { C::nope(); }       // both report
```

The same boundary holds for property receivers, promoted-property receivers,
return-typed call receivers, `@var`-annotated locals, and even a parameter
copied into another variable — Steins reports none of them, PHPStan reports all.
It applies uniformly to `call.undefined-method`, `call.too-few-arguments` and
argument-type mismatches: the receiver's type has to come from a same-scope
`new`, or the call has to be static.

This explains the drill's headline shape — one `call.undefined-method` in ~237k
files. It is also, honestly, what buys the zero-FP identity: binding
declaration-typed receivers is exactly the step that produces the
larastan-less Laravel false-positive mass measured in §5 below. Whether this
stays a documented design boundary or becomes N5/N6 work is an owner call; what
should not persist is leaving it undocumented, since it reads from the outside
like coverage that exists.

## 4. `is_vendor_path` is a `vendor` literal

`crates/steins-infer/src/lib.rs:704` tests for a path component named exactly
`vendor`. nextcloud vendors its dependencies into `3rdparty/`, so **456 of its
667 contract findings (68%) are other people's code** — aws-sdk-php,
pear-core-minimal and friends — and the one `debug.var-dump` on its default
surface is PEAR's. The drill record's nextcloud figure (662) is inflated by that
much.

Options: make the directory-name set configurable (`3rdparty`, `data/vendor`,
`lib/vendor`), or read composer's `vendor-dir` / `installed.json` when one
exists. The literal is right for the common case and wrong exactly where a
project predates or ignores composer.

## 5. What PHPStan's own volume is made of

Bucketed by cause (49,032 total): environment 22,644 (MISP's CakePHP 2 core is
an unchecked-out submodule; ec-cube2's runtime-generated `*_Ex` classes),
framework magic 3,950 + 18,112 (the same magic on app-owned subclasses:
Eloquent models, facades, PHPUnit test classes), PHPDoc quality 1,802,
mechanics 1,607.

Re-running with each app's own vendored framework extensions, same paths and
same level, measures how much of that is magic rather than defect (site-level
set difference):

| App | extensions | sites | erased by extensions |
|---|---|---:|---:|
| kimai | symfony + doctrine + phpunit | 136 | 103 (76%) |
| firefly-iii | larastan | 7,050 | 4,201 (60%) |
| BookStack | larastan | 955 | 451 (47%) |
| koel | larastan | 366 | 107 (29%) |
| mautic | symfony + doctrine + phpunit | 1,588 | 340 (21%) |
| ec-cube | doctrine | 263 | 5 (2%) |
| Sylius | symfony + doctrine | 984 | 1 (0%) |
| wallabag | (pinned to PHPStan 1.x) | — | extensions refuse to load on 2.2.5 |

What survives on the Symfony-side projects is mostly `varTag.nativeType` and
`generics.*` — annotations that disagree with the code, which is a real class of
finding, not noise. The lenient-default principle looks right in this light: the
tool that reports everything needs an ecosystem of extensions and a baseline
before its output is readable, and the projects here all carry both.

## Other real defects PHPStan surfaced that Steins has no check for

Declaration-incompatibility fatals — Sylius's three Doctrine test doubles,
`omeka-s` `FallbackRenderer::render()`, pixelfed `BearerTokenResponse::
getExtraParams()`. PHP fatals when these classes load; PHPStan's workers die on
them, which is how the operator learns. `throw.liskov-widened` covers `@throws`
clauses only, so Steins is silent. Out of the current scope, but worth a line in
the coverage map.

## Follow-up

1. Canonicalise paths in `collect_php_files` (item 1) — smallest fix, largest
   silent-loss surface; the repro script is a ready regression test.
2. Surface `parse_errors()` (item 2), and decide whether recovered trees may
   carry proof-grade findings.
3. Document the member-reach boundary (item 3) in the type specification, or
   schedule it.
4. Widen the vendor test (item 4) and re-measure nextcloud's contract count.
