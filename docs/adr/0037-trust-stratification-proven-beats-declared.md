# Trust stratification: a proven value never loses to a declared type

Real codebases carry coexisting type dialects — e.g. a DB-illusion zone
where `@return array{id: int}` historically means `int|numeric-string`
(PDO emulated prepares without stringify-off), working fine until the value
crosses into a strict boundary (typed JSON, strict comparison). Trusting
phpdoc as certain (PHPStan's `treatPhpDocTypesAsCertain` posture) adopts
the lie into inference and hides exactly those boundary breaks. Steins
does the opposite, in four parts:

1. **Trust order, explicit**: native declaration (runtime-enforced truth)
   > verified phpdoc (backed by all-call-sites proof) > unverified phpdoc
   (assertion). Iron rule: **inference never adopts a contract it has
   disproven** — when propagation proves `"123"` where `@return …int…` is
   declared, the lie is reported once at its source
   (`phpdoc.return-mismatch`) and the *truth* keeps flowing, so downstream
   boundary breaks stay detectable.
2. **PDO stringification is a pseudo-constant setting** (ADR-0008 family):
   `[runtime] pdo-stringify-fetches` in steins.toml declares the boot-time
   truth, switching the catalog's PDO fetch shapes to numeric-string —
   the illusion is not adopted; the runtime reality is declared and fed to
   inference (ask-the-real-thing, config edition).
3. **Boundary checks are a future policy profile**: "an illusion-zone
   value flows into a strict boundary (typed JSON output, strict
   comparison, typed property)" — writable precisely because true values
   propagate.
4. **Repair ships with detection** (ADR-0034 sibling transform): *phpdoc
   honesty repair* — widen a lying `@param int $id` to the observed proven
   union (`int|numeric-string`) from call-site evidence; the inverse of
   phpdoc→native promotion. Lying docblocks become machine-fixable debt,
   not scolding material.
