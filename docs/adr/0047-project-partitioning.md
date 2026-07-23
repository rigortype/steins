# Per-service project partitioning: scoped enumeration obstacles for transforms

The ADR-0043 closing measurement is the motivation, quantified: on the
legacy monorepo the fully-vouched sweep enumerated 23,148 promotion /
509 honesty candidates and transformed 0 — refusals dominated by
`dynamic-call-present` (17,390 / 354). That is the sound floor of
whole-universe reasoning (ADR-0034/0041): one `$o->$m()` anywhere opens
the caller set for everything everywhere. The monorepo's real shape —
~27 entry-point roots (service vhosts + a batch tree) over one
shared-lib tree, with the entry roots themselves almost free of textual
dynamism — is exactly the structure that reasoning throws away.
Partitioning is the recorded precision axis: obstacles stop being
project-global booleans and become *scoped* by what their site can
reach.

1. **Regions, declared not detected.** A run has four region kinds:
   *partitions* P₁…Pₙ (user-declared disjoint path sets — entry-point
   roots), *shared* S (vendor/ always, plus every first-party file no
   partition claims — the safe default direction: unclaimed code keeps
   whole-universe preconditions), *observers* O (declared; tests,
   dev-scripts — code that may reference any partition), and vendor V
   (inside S but with its own presumption, point 5). Declaration lives
   in steins.toml (point 7). Auto-detection (composer packages,
   top-level dirs) is rejected as the source of truth: the boundary is
   a claim with consequences, and the engine cannot verify the half of
   it that matters (point 3), so it must be owned by the user; a
   detection *report* that proposes partitions from directory structure
   and observed edges is deferred-with-design. The engine knows only
   the four region *kinds*: every path assignment is configuration —
   no directory name is ever hardcoded. Partitioning is
   transform-only in v1: with no `[transform.partitions]` section the
   universe is one region and every landed behavior is byte-identical.

2. **The scoping rule (the whole design in one sentence).** Every
   enumeration obstacle — `eval-present`, `dynamic-include-present`,
   the dynamic-call taint, each value-referenced name, each unresolved
   method/function name — is recorded with its *sites*, and blocks a
   candidate c only when c's declaring region is inside the site's
   **nameability set**: nameability(f∈Pᵢ) = Pᵢ ∪ S; nameability(f∈S) =
   U (everything); nameability(f∈O) = that file's referenced-region
   closure ∪ S ∪ O; nameability(f∈V) = V (point 5). A `$o->$m()` in
   partition A taints method names declared in A and in S — not B's
   internals. Per-name taints stay name-keyed and gain the same region
   scope. An unproven-payload `unserialize` in f widens f's nameability
   to U (class resurrection bypasses name reachability; ADR-0046 §3) —
   vouchable like any site.

3. **Soundness argument, split by trust stratum (ADR-0037/0046).** For
   a dynamic construct in A to invoke a symbol of B it needs the name
   or the receiver. (a) *Name reachability is VERIFIED*: the planner
   scans every cross-region reference — resolved calls, `new`,
   `extends`/`implements`/trait use, `instanceof`/`catch`, native type
   declarations, `::class`, and name-shaped strings/callables (the
   existing value-reference scan, region-attributed). Zero
   partition→partition and zero S→partition references is a checked
   fact, not an assumption. (b) *Receiver flow is USER-ASSERTED*: "no
   execution serves two partitions in one process" (separate entry
   points) is unverifiable statically — declaring partitions IS a vouch,
   and the completeness claim downgrades per the ADR-0046 valve:
   "conditional on the declared runtime disjointness of N partitions
   (+ M vouched sites)", carried in the report and EditPlan. Free-
   function candidates need only (a) — their partition claim is fully
   verified; method candidates need (b) too. (c) *Shared code is
   UNCLAIMABLE*: S runs inside every partition's runtime and can hold
   any partition's objects, so S-site dynamism taints U and S-declared
   candidates keep whole-universe preconditions. The asymmetry is the
   point: a service's private/final methods with all resolved callers
   inside the service are the unlock target; the shared-lib tree is not.

4. **Violations demote, they don't error.** A symbol of partition A
   referenced from S or from another partition (observer references
   excepted, below) makes its *file* effectively shared: the file is demoted to S (candidate
   role and taint role both), to a fixpoint, each demotion reported
   with its offending edge (`partition.demoted`). Hard-erroring instead
   would make partitioning unadoptable on a legacy tree; demotion is
   monotone toward the sound whole-universe posture. Observer references
   into partitions are legitimate (that is what observers are for) and
   demote nothing — observer call sites are ordinary enumerated callers.
   Observer files stay IN the candidate domain: transforms are
   operator-driven (dry-run → diff → approve, ADR-0020/0034), never
   transparent mass rewriting, so what to target is the operator's path
   argument, not a domain fiat. The vacuous-promotion hole (a
   framework's convention-reflection dispatch invokes test methods with
   no visible call site; a zero-observed-caller candidate would promote
   on an empty proof and fatal at runtime) is closed generically
   instead: **`no-observed-callers`** — a promotion candidate whose
   enumerated caller set is empty refuses, because a vacuous
   all-callers claim is zero evidence and cannot enter the verified
   stratum (ADR-0037; amends the ADR-0041 §3 taxonomy). No framework
   knowledge required; honesty already refuses empty evidence by
   construction.

5. **Vendor dynamism presumption** (new; extends ADR-0046's vendor
   include presumption). Vendor-interior dynamic dispatch is presumed
   to target vendor code plus callbacks first-party code registered —
   and literal registrations (`[$this, 'm']`, first-class callables,
   callable strings) are already caught name-wise by the value scans,
   while non-literal ones already raise the dynamic taint in the
   registering region. The documented residual: frameworks invoking
   first-party methods by *naming convention* via reflection (a test
   runner's `test*` — absorbed by point 4's `no-observed-callers`
   refusal; a framework's magic dispatch — magic methods are never
   candidates, ADR-0046 §3). Rebuttable per steins.toml; without this presumption
   the ~54k-file vendor tree keeps the floor at zero and partitioning
   measures nothing.

6. **Mechanism: one salsa Project, no engine change.** N Projects are
   rejected: the edge verification (point 3a) must SEE cross-partition
   references, S would be re-parsed N times (~84k files), and the
   post-check stays whole-universe. The region map is a pure function
   of config + file path, built in `steins-edit` at planning time — the
   salsa `Project`, `project_index`, and the checker are untouched. The
   sweeps (`steins-infer/src/promote.rs`) change shape only: the
   `any_dynamic_*` booleans become site lists and the name-taint sets
   become name→sites maps, so the planner can region-attribute every
   obstacle; with one region the planner degenerates to today's
   behavior. The checker stays whole-universe: diagnostics don't rest
   on caller completeness, and zero-FP gains nothing from boundaries —
   partitioning is a transform-precondition concept in v1.

7. **Config surface** (ADR-0020/0023 conventions; globs as in
   `[paths.sets]`):

   ```toml
   [transform.partitions]
   observers = ["tests/**", "dev-script/**"]

   [transform.partitions.sets]
   svc-a = ["svc-a.example/**"]
   svc-b = ["svc-b.example/**"]
   batch = ["batch/**"]
   ```

   Overlapping partition globs are a config error; unmatched first-party
   files are shared (no `shared` key — one source of truth). The
   `[transform.vouch]` valve generalizes uniformly: any obstacle site
   (now including dynamic-call and unserialize sites) may be vouched,
   with the same claim downgrade. fp-gate is unaffected (corpus
   packages are already one-universe-each); the legacy-monorepo
   measurement carries a `partitions` table in `corpus.local.toml`
   mapped onto the same shape. The report groups candidates and
   obstacles by region; no new CLI flag in v1.

8. **Predicted measurement** (recon-grounded; the implementation is
   judged against this). Candidate mass tracks docblock mass:
   the shared-lib tree holds ~26k `@param` tags, entry-point roots
   ~5–6k, observers ~5k — so on the order of **3,000–4,000 of the
   23,148** promotion candidates (and ~50–100 of the 509 honesty
   candidates) are partition-resident and should stop refusing for
   `dynamic-call-present`/`eval-present` once scoped (most service
   roots are textually dynamism-free; the batch tree's one `eval` and
   the shared tree's two dynamic calls are vouchable point-fixes).
   That is the metric this ADR moves. End-to-end transformed counts
   stay far smaller under v1's literal-argument proofs
   (`argument-not-proven` becomes the next dominant refusal, by
   design); fold/dataflow-backed proofs are the already-recorded
   independent axis (ADR-0041 §1).

9. **Deferred-with-design**: cross-partition call contracts (a declared
   export surface so partition→partition calls through a named API
   don't demote — the boundary as an interface, Liskov-checked);
   checker-side partition diagnostics (boundary-violation lint,
   dead-export detection) — requires promotion of the region map into
   engine config; class-visibility taint refinement (a private method
   is dynamically callable only from its own class's scope, so foreign
   dynamism could be discounted for private candidates even inside one
   region — needs a Reflection posture first); per-partition honesty
   evidence (widening an S-declared doc from one partition's callers
   can never claim the verified stratum — stays whole-universe);
   partition auto-detection reports (point 1).
