# Catalog seeding is demand-driven; the initial FP-gate corpus

**Catalog (ADR-0014) seeding order**: type signatures are generated
mechanically from php-src stubs in bulk; **effect coloring follows measured
demand** — builtin call frequency counted over the FP-gate corpus, colored
top-down. Uncolored functions widen to unknown-effect (a miss, never an FP),
the only seeding order compatible with the zero-FP bar. The by-ref
out-parameter survey (79 functions) seeds conditional purity; the
pseudo-constant settings table (ADR-0008) comes from the same notes.
Extension modules are added on demand, driven by what the sidecar's
`ReflectionExtension` audit reports actually loaded.

**Initial FP-gate corpus (ADR-0013 concretized)** — balanced across
io-heavy, pure-computation, and metaprogramming-heavy code:

- composer/composer — scale, real-world complexity
- phpunit/phpunit — reflection and metaprogramming stress
- guzzlehttp/guzzle — `io.net.http`, PSR-7
- monolog/monolog — `output`/`io.fs`, handler abstractions (envelope
  carriers in the wild)
- symfony/console, symfony/process — component culture, `io.process`
- league/flysystem — `io.fs` behind interfaces
- nikic/php-parser — large pure-computation library (the `Pure` side)
- nesbot/carbon, cakephp/chronos — `nondet.time` heartland; exercises
  pseudo-constant timezone settings
- the private monorepo, via the ADR-0013 injection point
