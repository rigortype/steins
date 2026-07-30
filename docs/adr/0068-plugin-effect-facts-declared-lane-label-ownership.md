# Plugin effect facts live in the declared lane; label roots follow the vendor name

Issue #68. Amends ADR-0039 (plugin contract) where effect facts are
concerned; consumes ADR-0067's proven/declared vocabulary. Status: PENDING
ratification (autonomous design under the owner's post-hoc-ratification
mode). ADR-0039 already fixes distribution (`type: steins-plugin`),
discovery, the subscription model, the phpdoc-string wire format, and the
merge rules for *declarations*. Three edges were left open for *effect*
facts; this ADR closes them.

## 1. Which lane a plugin's effect fact enters

**Decision: the declared lane, always — and without taint discharge.**

A plugin coloring (`Redis::get` is `io.redis`; a SendGrid call is
`email.send`) is a third-party assertion. ADR-0039 v1 already excludes
plugin *diagnostics* because the zero-FP banner cannot vouch for third-party
quality; the same argument decides the lane. A proven effect can manufacture
an `effect.envelope-exceeded` finding, so plugin facts must never enter the
proven lane — otherwise a plugin bug becomes a Steins false positive, which
is the one budget this project refuses to spend.

The taint question separates plugins from ADR-0067's envelope import. An
interface envelope discharges the covered call's exhaustiveness taint
because it is a **checked** contract: every analyzed implementation is held
to it by `effect.liskov-widened`. Nothing checks a plugin assertion. So a
plugin-covered call keeps its taint: the summary reads "declared `io.redis`,
and possibly more", which is exactly the truth of an unchecked claim.
ADR-0067 rejected bound-plus-taint as double counting *for the checked
stratum*; for an unchecked stratum it is not double counting, it is the
trust order speaking. Discharge is therefore a property of the checked
stratum, not of the declared lane.

Two consequences, stated so nobody re-derives them wrong:

- **Semantic layering is additive.** `io.net.http` proven by the transport
  catalog and `sendgrid.mail.send` / `email.send` declared by a plugin
  coexist on one call, each in its lane. A plugin can never remove, narrow,
  or re-lane a proven effect — refine, never weaken, imported unchanged
  from ADR-0039's declaration rules.
- **Extension-class coverage stays honest.** For a class source descent
  cannot see (ext-redis), the plugin's colors are the *only* information,
  and they surface as declared-with-taint rather than masquerading as
  proven. When ADR-0014 mining later proves the same family natively, the
  native rows enter proven and the plugin's copy normalizes away
  (ADR-0067 rendering rule).

## 2. Label namespace ownership

**Decision: a plugin registers labels only under its own vendor root, or as
descendants of core taxonomy roots; the project may register anything.**

The registry is a set union, so duplicate registration of one label string
is mechanically harmless — the collision that matters is *meaning*, which
Steins cannot judge. Ownership therefore has to be a naming rule enforced at
load, not a semantic check:

- The **core roots** (`exit ffi global io mutate nondet output failure`) are
  Steins' own. A plugin may register **descendants** (`io.redis`,
  `io.db.dynamo`) — subsumption then works with no new machinery, which is
  why descendants are the recommended spelling for anything transport-like.
- A **new root** must equal the plugin's composer **vendor name**
  (`sendgrid/steins-plugin` may register `sendgrid.*`). ADR-0060 chose the
  packagist vendor name deliberately; this makes that choice the anti-squat
  rule: packagist already adjudicates who is `sendgrid`, and Steins inherits
  the verdict instead of running its own registry.
- **Cross-vendor vocabularies** (`email.send`, `acme.*`) belong to the
  project: `steins.toml` label registrations are unrestricted, and an
  explicitly-listed plugin (ADR-0039: explicit listing wins) is trusted the
  same way — the owner's listing is the vouching act. Auto-discovered
  plugins get the vendor-prefix rule only.
- A registration violating the rule is **rejected and reported by name**
  (the plugin loads otherwise) — same posture as the api-version gate:
  silence names itself.

## 3. Fact versioning against the described package

**Decision: composer constraints are the versioning mechanism; facts carry
no in-band version predicates in v1.**

A plugin describes the package versions it `require`s (or declares
`conflict` against): if `sendgrid/steins-plugin` speaks about
`sendgrid/sendgrid ^8`, it says so in its own composer.json, and composer —
which already solved this problem — refuses the install where the facts
would not apply. The environment fingerprint (composer.lock hash + plugin
versions, ADR-0039) already keys the response cache, so a version change
re-asks the plugin. Inventing a per-fact version predicate in the wire
format would duplicate composer's solver with a worse one; rejected. If a
single plugin must serve disjoint major versions with different facts, that
is what the sidecar boot answering *at runtime* is for — the plugin can ask
the installed package for its version and answer accordingly.

## Rejected

- **Plugin facts in the proven lane behind a trust flag.** A flag on a
  proven label is ADR-0067's rejected origin-flag design returning through
  the side door; every diagnostic would need to consult it.
- **Taint discharge for explicitly-listed plugins.** Listing vouches for
  *identity*, not correctness; discharge stays a checked-stratum property.
  Revisit only if plugin assertions ever gain a verification mechanism.
- **A Steins-run label-name registry.** Packagist plus the vendor rule is
  the same guarantee without operating anything.
