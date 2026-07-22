# Folding runs outside the salsa query graph (for now)

Salsa queries must be deterministic functions of their inputs; a fold is an
IPC call into an external PHP process. Rather than pretending the sidecar is
pure, the check pass that may fold runs as a plain function *outside* the
query graph — parse and function indexing stay memoized salsa queries, and
fold results are memoized per run in a plain map keyed by
`(function, args)`. For allowlisted pure+deterministic builtins the fold IS
referentially transparent, so this placement loses no correctness, only
cross-run incrementality of folded findings.

Revisit trigger: when LSP incrementality makes re-folding on every
keystroke measurable, fold results move into the graph as an explicit salsa
input layer (recorded facts fed back into queries), not as hidden impurity
inside a tracked function. The invariant either way: a fold that fails —
timeout, crash, unknown function — widens and never invalidates
memoized analysis (ADR-0024's never-a-wrong-diagnostic rule).
