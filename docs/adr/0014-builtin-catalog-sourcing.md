# Builtin catalog: php-src stubs as base, Steins effect layer on top, phpstorm-stubs as PECL supplement

Three layers, trust decreasing outward:

1. **php-src `.stub.php`** — the base for type signatures of bundled
   builtins/extensions: the primary source the engine itself is generated
   from, tagged per PHP version (covers ADR-0011's 8.1+ range). The catalog
   version of the native-declaration philosophy.
2. **Steins layer** — effect coloring (ADR-0008) and value-precision
   metadata (by-ref out parameters, conditional purity, folding
   eligibility). This layer is Steins' own asset; it exists nowhere
   upstream. **Effect/purity information is authored here exclusively** —
   in particular, phpstorm-stubs' `#[Pure]` markings are known-unreliable
   (see JetBrains/phpstorm-stubs PR #1724, #1730) and are never imported;
   Steins reclassifies purity from scratch.
3. **phpstorm-stubs** — supplement for the PECL world (redis, imagick, …):
   type signatures only, demoted to "no quality responsibility accepted,"
   never a source of effects.

The sidecar audits the catalog continuously: the project PHP's
`ReflectionFunction`/`ReflectionExtension` is diffed against catalog entries,
so staleness and build-configuration drift are detected by asking the real
thing (ADR-0004's asset, cashed a third time) instead of rotting silently the
way hand-maintained function maps do.
