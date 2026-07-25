# The Packagist vendor is `typedduck`, not the namespace and not the GitHub org

The PHP namespace is top-level `Steins\`, so `steins/` would be the tidy vendor
name. It is not available: `steins/collections` (Steins-framework, an Illuminate
Collections fork) holds it, and `stein/` is held by `stein/stein`, a
FrankenPHP-oriented micro-framework. Both are small — six and twelve downloads —
but a Packagist vendor namespace belongs to whoever registered it, and neither
is abandoned. The tidy answer is simply gone.

**Decision: publish under `typedduck/`.** `typedduck/steins` and
`typedduck/steins-attributes`.

## Why not the two alternatives

**`php-steins/`** tracks the software name (PHP;STEINS) most closely and is
free. Against it: `php-steins/steins` is redundant on its face and
`php-steins/steins-attributes` doubly so, and — the real cost — it makes the
vendor name a *product* name. A team that names vendors after products
accumulates one vendor per product, and this team already publishes
`typedduck/consult-rector`. Two vendors for one team is a question users have to
answer ("is this the same people?") for no benefit.

**`rigortype/`** matches the GitHub organization, which is what this repository's
manifests, workflows, and Homebrew tap already say, and it is what an earlier
draft of the Composer work assumed. It is wrong for a reason that only turned up
on inspection: **`typedduck/consult-rector` is hosted at
`github.com/rigortype/consult-rector`.** The team has already decoupled vendor
from organization, and already decided which of the two names goes on Packagist.
Choosing `rigortype/` here would contradict a live precedent and split the
brand across two vendors on the basis of a coincidence — that the org happened to
be named first.

The asymmetry underneath it: **a GitHub organization can be renamed or its
repositories transferred; a Packagist vendor name is permanent in every
`composer.lock` ever written against it.** Identity that has to survive decades
of lockfiles should be the brand, not the hosting account. The copyright line
the packages carry — `TypedDuck, USAMI Kenta <tadsan@zonu.me>` — already names
which of the two is the durable one.

## On the namespace mismatch

`typedduck/steins` autoloading `Steins\` is unremarkable, not a compromise. The
vendor field identifies the *publisher*; the namespace identifies the *code*.
The ecosystem's own practice, sampled from packages that are not obscure:

| package | namespace |
| --- | --- |
| `nikic/php-parser` | `PhpParser\` |
| `carthage-software/mago` | `Mago\` |
| `laravel/framework` | `Illuminate\` |

The last is the strongest evidence: Laravel is a brand with enormous incentive
to align the two, and does not. `carthage-software/mago` is the closest analogue
to our own case — the same shape of tool, distributed the same way, with the
same split between company vendor and product namespace.

## Consequences

- Both `composer.json` files, and every document, workflow, and skill reference
  naming a Composer package, change from `rigortype/` to `typedduck/`. Nothing
  is published yet, so this costs an edit pass and no user migration. **This is
  the last moment at which that is true** — after the first release, changing a
  vendor means abandoning the old package and republishing, and every consumer
  edits their `composer.json` by hand.
- The GitHub organization stays `rigortype`. Repository URLs, the issue tracker,
  the release archives, the Homebrew tap (`rigortype/tap/steins`) and the
  `cargo install --git` fallback are unaffected — none of them is a Packagist
  vendor. The README and quickstart already name the repository and the package
  separately, so no prose becomes wrong.
- The namespace stays `Steins\`. It is the analyzer's recognized attribute
  identity (`steins\pure`, `steins\effect`, case-folded, in
  `crates/steins-syntax`), and changing it would be a breaking change to user
  source code in exchange for cosmetic agreement with a vendor field.
- The Rust crate names (`steins-*`) and the binary name (`steins`) are
  untouched; they are in a different registry namespace with a different
  occupancy situation, and ADR-0025 already records why crates.io is not a
  channel at all.
- If `steins/` is ever released — the holder is at v0.0.2 with six downloads —
  it does not reopen this. Moving to it later would cost every consumer a manual
  edit, and would re-split the team's vendors to buy an alignment this ADR finds
  is not worth buying.
