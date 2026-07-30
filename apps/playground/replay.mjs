// The replay fixpoint driver (ADR-0066 §2), transport-agnostic and
// dependency-free. The frontend runs it over two Web Workers; `smoke-replay.mjs`
// runs it over a wasm module and a php-wasm instance in one Node process. One
// implementation, so the browser loop and the loop the smoke suite pins are the
// same loop.
//
// The contract in one paragraph: an analysis answers every fold/reflect/env
// question from a supplied table and reports the ones it could not answer as
// `pending`. A run with non-empty `pending` is NoFold-degraded — sound but less
// precise — and MUST NOT be rendered. Answer the pending keys, put the answers
// back under the same key strings, run again. The answered set strictly grows,
// so the loop terminates; the cap below is defensive, against an answerer that
// keeps returning something the analysis rejects.

// Deliberately generous: the flagship converges in two iterations, and the
// question set a snippet can generate is finite and small.
export const ITERATION_CAP = 32;

/**
 * Drive one analysis to its fixpoint.
 *
 * @param analyze  (table) => { ok, pending, value } — one replay iteration.
 * @param answer   (keys)  => { answers, failures }  — the engine, batched.
 * @param table    the session-global answer table, mutated in place. Answers are
 *                 pure functions of their request key (ADR-0066 §2), so entries
 *                 stay valid across analyses and the table only ever grows.
 * @param aborted  () => boolean — the caller's staleness guard, polled after
 *                 every await.
 * @param cap      iteration cap.
 *
 * @returns { status, value, iterations, reason }, where `status` is
 *   "converged" — `value` is a complete run and is the ONLY status whose value
 *                 may be rendered;
 *   "aborted"   — a newer run superseded this one;
 *   "stalled"   — the analysis re-asked something already in the table;
 *   "exhausted" — the cap ran out;
 *   "failed"    — the analysis errored, or the engine did.
 * Every status but "converged" means: fall back to the plain, engine-free call.
 */
export async function driveReplay({ analyze, answer, table, aborted = () => false, cap = ITERATION_CAP }) {
  for (let iterations = 1; iterations <= cap; iterations++) {
    const run = await analyze(table);
    if (aborted()) return { status: "aborted", iterations };
    if (!run.ok) {
      return { status: "failed", value: run.value, iterations, reason: "the analysis returned an error envelope" };
    }
    if (run.pending.length === 0) return { status: "converged", value: run.value, iterations };

    const asked = run.pending.filter((key) => !(key in table));
    if (asked.length === 0) {
      // The table already holds every pending key, so answering again would ask
      // the same question forever: the answer is one the analysis will not take.
      return { status: "stalled", iterations, reason: `${run.pending.length} request(s) re-asked despite an answer` };
    }

    let batch;
    try {
      batch = await answer(asked);
    } catch (err) {
      return { status: "failed", iterations, reason: String(err && err.message ? err.message : err) };
    }
    if (aborted()) return { status: "aborted", iterations };
    if (batch.failures.length > 0) {
      return { status: "failed", iterations, reason: `the engine could not answer ${batch.failures.length} request(s)` };
    }
    for (const key of Object.keys(batch.answers)) table[key] = batch.answers[key];
  }
  return { status: "exhausted", iterations: cap, reason: `no fixpoint within ${cap} iterations` };
}
