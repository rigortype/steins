# Diagnostic Shape

**Status: implemented.** ADR-0022, ADR-0050.

## The value

```rust
pub struct Diagnostic {
    pub id: &'static str,        // a registry id
    pub path: String,            // the diagnostic path
    pub line: u32,               // 1-based
    pub column: u32,             // 1-based
    pub message: String,
    pub facet: Option<Facet>,    // registry-declared, additive
}
```

Deliberately **flat**, so the CLI can render text or JSON without knowing
anything about the analysis. `id` is `&'static str` because the id set is closed
and compile-time known.

`facet` takes part in equality and hashing, harmlessly: two findings that were
previously equal share an origin file and offset and so compute the same facet.
Findings are deduplicated by structural equality before display.

The **layer** is not a field. It is looked up from the registry by id
(`steins_infer::layer(id)`), which is what makes the registry the single source
of truth rather than one of two.

## The registry and its totality tests

```rust
pub const DIAGNOSTIC_REGISTRY: &[(&str, Layer)];   // id -> layer
pub const ALL_EMITTABLE_IDS: &[&str];              // ids reaching a construction site
pub const REGISTERED_NOT_YET_EMITTED: &[&str];     // registered ahead of emission
```

The workspace totality test (`tests/registry.rs`) asserts:

1. `DIAGNOSTIC_REGISTRY` and `ALL_EMITTABLE_IDS ∪ REGISTERED_NOT_YET_EMITTED`
   are the same set, both directions;
2. the two lists are **disjoint** — an id emitted for the first time must leave
   `REGISTERED_NOT_YET_EMITTED`;
3. every id in `REGISTERED_NOT_YET_EMITTED` is actually registered.

The lists live in different files on purpose: the registry carries the *layer*
attribute, `ALL_EMITTABLE_IDS` carries *is emitted*, and the test binds them. A
new emitter whose id is added to one but not the other fails to build the tests.
Registering an id without a layer does not compile — every entry is an
`(id, Layer)` tuple.

`Layer` has **four** variants: `Proof` (runtime survivability, zero-FP, gates
red on sight), `Contract` (declared-contract acceptance, increase tripwires),
`Mechanics` (apparatus hygiene, red on sight, suppression-exempt), and `Debug`
(ADR-0053: requested introspection — an *answered question*, displayed on every
profile but baseline- and suppression-exempt and excluded from every gate
counter; a dump is not a finding).

`REGISTERED_NOT_YET_EMITTED` today holds `call.undefined-function` and
`class.undefined` (the existence ids, waiting on their dam-gated stages),
`call.too-many-arguments` (fires for internal targets only, so it waits on the
sidecar `reflect` slice), and `debug.var-dump` (ADR-0053 D4, landing in
v0.1.0). The dump surface's explicit pair — `debug.type` /
`debug.phpdoc-type` — lit up at D3 and is emittable.

Semantics of layers, facets, prefix matching, and profiles:
[`diagnostic-policy.md`](../type-specification/diagnostic-policy.md).

## The display pipeline

Ordered, in the CLI (ADR-0050 §6):

```text
inference → vendor filter → profile surface → [[policy]] → inline ignores → baseline → format
```

- **vendor filter** — `is_vendor_path` matches whole path components split on
  both `/` and `\`, so `vendor_proj/` and a file named `vendor.php` are not
  vendor. Suppressed unless `--vendor-diagnostics`.
- **`[[policy]]`** — present as a no-op stage with a seam; **not implemented**.
- **inline ignores** — `steins_infer::apply_inline_ignores`, which also emits
  the two mechanics meta-diagnostics. They are exempt from every channel.
- **baseline** — see [baseline.md](baseline.md).

Counts of what each stage removed are reported, so suppression is never
invisible.

## The text rendering

```text
src/Timesheet/TimesheetService.php:149:13: error[throw.undeclared]: <message> — proven escape
```

`path:line:column` is the first field so editors and terminals can jump to it.

## The JSON rendering

```json
{
  "findings": [
    {
      "id": "throw.undeclared",
      "layer": "contract",
      "level": "fail",
      "path": "src/Foo.php",
      "line": 149,
      "column": 13,
      "message": "…",
      "origin": "direct"
    }
  ],
  "profile": "throws-direct",
  "vendor_suppressed": 0,
  "suppressed": 0,
  "baselined": 0
}
```

`layer` and `level` are additive fields (ADR-0050 §2/§7). A facet appears as its
own key (`"origin"`) **only** on ids that declare one. Every emitted id is
registered, so `layer` is always present.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | no `fail`-level finding was displayed |
| 1 | at least one `fail`-level finding was displayed |
| 2 | usage or config error |

`warn`-level findings are exit-neutral by construction. The `debug` layer is
excluded from every gate counter.

## Not implemented

- **`sarif` and `github` formats** (ADR-0054), with CI auto-detection. The
  binding rule when they land is **format invariance**: for a fixed invocation
  all formats render the same displayed finding multiset and the same exit code.
  Nothing format-specific may reopen a suppression channel (a baselined finding
  must not reappear as a SARIF "suppressed result") or drop a displayed finding
  (no annotation cap).
- **`doctor`** — the posture report (ADR-0054). Its minimal form is in scope
  for the v0.1.0 landing point; no code exists as of this writing.
- **Fix-it payloads** on diagnostics (ADR-0010).
- **A stable message contract.** Messages are prose and keep improving; they are
  explicitly not a suppression key (ADR-0023).
