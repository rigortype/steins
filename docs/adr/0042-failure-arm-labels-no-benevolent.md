# Failure-cause labels on union arms: the benevolent-union replacement

ADR-0030 erased `__benevolent<T1|T2>` to plain `T1|T2`: benevolent
unions compensate for worst-case-reporting noise, and a proof layer that
acts only on proven values has no such noise to suppress. The residual
question — how `curl_init(): CurlHandle|false` behaves once the catalog
carries builtin return shapes and the boundary profiles (ADR-0037 point
3) arrive — is answered by classifying failure arms by *cause* (a fact
the catalog can state), never by probability (a judgment it cannot):

1. **Input-determined** — `preg_match` (malformed pattern),
   `json_encode` (unencodable value). Not a label problem: with proven
   arguments the false arm is *disproven per call site* via conditional
   return shapes, and foldable builtins get the exact answer from the
   sidecar (ADR-0004) — proven-beats-declared applied to the catalog.
   `failure.input` exists only as the fallback label for sites whose
   arguments stay unproven.
2. **Resource-exhaustion** — `curl_init`, `imagecreate*`: false means
   allocation failure; unrecoverable in practice, statically
   irrefutable. Label: `failure.resource`.
3. **Environment-operational** — `fopen`, `fsockopen`: false is a
   normal operational outcome; not checking it is a real bug. Label:
   `failure.environment`.

Mechanism: the labels are ADR-0038's reserved value-provenance labels
(dot-path, ADR-0018 registry + prefix subsumption under `failure.*`),
attached to the failure arm's values by catalog rules. They are
provenance, not value properties — two `false`s differ only in origin —
so they live outside the Refined layer, preserving ADR-0035
extensionality, exactly as ADR-0038 requires.

Behavior per layer:

- **Catalog states the truth, always**: the return shape stays the
  honest `CurlHandle|false`. Deleting the arm would move the benevolent
  lie into the catalog — `treatPhpDocTypesAsCertain` committed in the
  opposite direction.
- **Runtime layer**: unchanged — silent unless a break is proven; the
  fp-gate posture is unaffected.
- **Boundary profiles (future)**: `boundary.unchecked-failure`-family
  diagnostics consume the labels. The default profile exempts
  `failure.resource` arms from must-check and includes
  `failure.environment`; the strict profile includes both. "curl_init
  needs no check, fopen does" is thereby policy, not semantics — the
  same resolution shape as the throw.undeclared profile-demotion
  candidate.

Registered as the benevolent-union replacement in ADR-0030's divergence
registry (honest union + failure-cause labels + policy-profile
consumption), and available as upstream discussion material.
