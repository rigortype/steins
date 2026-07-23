# Position queries: replay over retention

Cursor-position member completion — the LSP capability this project is
designed never to paint out — decomposes as: the facts of `$x` at offset
N in file F, then the members of that type. The second half has existed
since ADR-0043 (project index, trinary is-a, `TypeMember::Instance`).
The first half does not exist: facts live in a transient env inside a
linear scope walk, the check pass runs outside the salsa graph
(ADR-0028), and nothing of inference is memoized across runs. Semantic
features (narrowing, generics carry, callable signatures, packs) are
queued to land *before* any LSP work — so what binds now is the minimum
that keeps position queries reachable, and nothing more.

1. **Decision: replay, not retention.** "Facts of `$x` at offset N" is
   answered by re-walking the enclosing scope from a memoized
   per-declaration entry state, stopping at N. Rejected: (a) retained
   position-indexed fact tables — order 10⁶–10⁷ program points on the
   ~30k-file scale, gigabytes of state whose invalidation would itself
   require replay machinery; (b) whole-project re-inference per query —
   the batch pass is minutes-scale. A scope re-walk is O(scope size)
   given its entry state — microseconds-to-milliseconds for real
   function bodies — and the annotate path already demonstrates the
   walk surfacing facts mid-walk (the `LineFact` threading).

2. **Constraint (binds every change from now): scope-walk
   replayability.** The walk of any scope must remain a deterministic
   function of (the scope's CST, its entry state, salsa query answers,
   the fold memo). No feature may couple mid-walk state across scopes
   except through the entry state or a query. Design-time litmus for
   every new inference feature: *could this scope's walk be re-run
   alone, later, and produce identical facts?* If not, the feature is
   redesigned before it lands.

3. **Constraint (binds every change from now): a canonical entry state
   per declaration.** Position queries need one well-defined seeding, not
   "whichever descent ran last": declared envelopes (authoritative,
   ADR-0037 trust order), refined by observed-caller evidence only
   where enumeration is exhaustive, non-exhaustiveness carried
   explicitly — the annotate `…?` discipline promoted to architecture.
   A feature introducing a new fact kind defines that kind's
   contribution to the entry state when it lands, not retroactively.

4. **Constraint (binds every change from now): no global-ordering
   dependence.** No fact or diagnostic may depend on the iteration
   order of a whole-project pass. Ordering may remain presentational
   (output sorting, dedup) — never semantic.

5. **Prerequisite, scheduled not yet binding: inference enters the
   query graph** (ROADMAP M5). Per-declaration entry states become
   memoized salsa queries; `project_index` shards per-symbol (the plan
   recorded on its granularity note, ADR-0009); fold results become
   recorded salsa inputs — ADR-0028's revisit trigger, restated here
   as an entry gate for LSP work. Until then the monolithic batch pass
   stays legitimate for the CLI.

6. **Packaging, decided minimally**: `steins lsp` stdio subcommand
   (ADR-0020) backed by a dedicated `steins-lsp` crate that owns
   protocol types the way `steins-syntax` owns Mago — no protocol type
   leaks into analysis crates. Whether protocol types are hand-rolled
   over the shared JSON-RPC/NDJSON plumbing family (ADR-0024) or come
   from a vetted crate is an implementation decision at build time,
   isolated behind that crate boundary either way.

7. **Not decided here**: the LSP feature set beyond
   diagnostics/hover/member-completion, completion ranking, cross-
   session index persistence, cancellation, position encoding.
   Span-keying the fact surface (today line-keyed `LineFact`) is
   mechanical M6 work, not a present constraint.
