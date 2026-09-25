// Which profiles the findings panel points at, composed from an envelope's
// `hidden` object: per other built-in profile, how many findings it would list
// that the selected one does not (a set difference, computed by the module).
//
// Extracted from `index.html` for the reason `boundary.mjs` was: what the page
// SAYS is checkable by the smoke suite without a DOM.
//
// The margin overlay is profile-blind — it marks every finding the analysis
// proves — so without this line a `✗` there can sit beside an empty panel. The
// line names only the profiles worth a click:
//
//   * `contracts` and `strict`, the rungs of the ladder. `default` and
//     `throws-direct` never appear: `contracts` shows everything either of
//     them does, so neither can add anything the ladder does not name.
//   * `pedantic` only when it adds something the ladder does not. It is
//     `contracts` plus the house-style ids, so its count minus `contracts`'
//     (zero when `contracts` is the selected profile, since the selected one is
//     never listed) is exactly what only `pedantic` shows. From `default` it
//     would otherwise repeat the contract findings a third time.

const LADDER = ["contracts", "strict"];

/**
 * @param hidden  the envelope's `hidden` object (absent on an error envelope)
 * @returns [{ profile, count }] in the order the page lists them; empty when
 *          the selected profile hides nothing worth pointing at.
 */
export function hiddenSteps(hidden) {
  if (!hidden || typeof hidden !== "object") return [];
  const count = (name) => (Object.hasOwn(hidden, name) && Number(hidden[name])) || 0;
  const steps = LADDER.filter((name) => count(name) > 0).map((name) => ({ profile: name, count: count(name) }));
  if (count("pedantic") > count("contracts")) steps.push({ profile: "pedantic", count: count("pedantic") });
  return steps;
}
