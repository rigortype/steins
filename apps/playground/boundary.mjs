// The precision-boundary panel's content, composed from a `boot` object.
//
// Extracted from `index.html` for the reason `replay.mjs` and `php-dispatch.mjs`
// were: what the page says about the engine is now checkable by the smoke suite
// without a DOM. The panel had a defect only a rendering test could catch — it
// grouped the refused rows by axis and then quoted the FIRST row's witness for
// the whole group, so nine of eleven recorded divergences never reached a
// reader, while the boot object carrying all eleven looked perfect.
//
// Nothing here re-derives a gate. Every claim is a field of `boot`, which is the
// engine surface as the shared fold policy sees it (ADR-0066).

/// The framing sentence for a refusal axis. A row's own `witness` says what
/// diverged; this says what KIND of difference produced it.
///
/// Unknown axes get the neutral line rather than a guess: the catalog's
/// `RefusalAxis` grows when a probe finds a new kind of divergence, and a page
/// that assumed "not the word size" meant "compiled differently" would state a
/// falsehood about an ini- or OS-shaped row the day one lands. The point of
/// carrying the axis as data was that the page stops writing its own reasons.
const AXIS_FRAMING = {
  integer_width: (bits) =>
    `produce or render an integer in the machine's own word, and this build's word is ${bits}`,
  build_option: () =>
    `depend on how the engine was compiled — same version, same ini, different build`,
};

const escHtml = (s) =>
  String(s).replace(
    /[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c],
  );

const bitsOf = (b) => (b.int_size ? `${b.int_size * 8}-bit` : "of unreported width");

/// One `<li>` per refused row: the name, and the divergence recorded for it.
/// Every row gets its own witness — that is the whole discipline the catalog
/// enforces on itself (`every_refused_row_carries_its_witness`), and quoting one
/// per group threw most of it away.
function refusedItems(b) {
  const rows = b.refusals ?? [];
  const byAxis = new Map();
  for (const r of rows) {
    if (!byAxis.has(r.axis)) byAxis.set(r.axis, []);
    byAxis.get(r.axis).push(r);
  }
  const items = [];
  for (const [axis, rs] of byAxis) {
    const framing = AXIS_FRAMING[axis];
    const names = rs.map((r) => `<code>${escHtml(r.name)}()</code>`).join(", ");
    const head = framing
      ? `${names} <strong>do not fold</strong> — they ${framing(bitsOf(b))}.`
      : `${names} <strong>do not fold</strong> — a divergence is on record for each.`;
    const witnesses = rs
      .map((r) => `<li><code>${escHtml(r.name)}</code> — <code>${escHtml(r.witness)}</code></li>`)
      .join("");
    items.push(`${head}<ul>${witnesses}</ul>`);
  }
  // A refused name with no reason in `boot` would vanish from the panel
  // entirely, so name whatever the grouping missed rather than dropping it.
  const shown = new Set(rows.map((r) => r.name));
  const rest = (b.refused_folds ?? []).filter((n) => !shown.has(n));
  if (rest.length) {
    items.push(
      `${rest.map((n) => `<code>${escHtml(n)}()</code>`).join(", ")} <strong>do not fold.</strong>`,
    );
  }
  return items;
}

/**
 * Compose the panel from a boot object.
 *
 * @returns {{live: string[], off: string[], framing: string} | null} — `null`
 *   when there is no engine to describe, which is the caller's cue to hide the
 *   panel rather than render an empty one.
 */
export function composeBoundary(b) {
  if (!b || !b.php_version) return null;

  const live = [];
  live.push(
    b.fold_lane === "full"
      ? `<strong>Folded values</strong> from the real engine — all ${b.fold_total} builtins on the folding allowlist.`
      : `<strong>Folded values</strong> from the real engine — ${b.fold_portable} of the ${b.fold_total} builtins on the folding allowlist: the ones a ${bitsOf(b)} engine is verified to answer exactly as a 64-bit one does. <code>str_repeat("ab", 2)</code> is <code>'abab'</code> here, as on the CLI.`,
  );
  live.push(
    `<strong>Reflected return envelopes</strong> — a builtin's declared return type, read back from this engine rather than guessed (<code>strtoupper($x)</code> is <code>uppercase-string</code> once the predicate transfers refine it).`,
  );
  if (b.absence_family) {
    live.push(
      `<strong>The absence family</strong> — <code>call.undefined-function</code> and its siblings, witnessed against ${escHtml(b.label ?? "the running engine")}. Without an engine these are structurally silent.`,
    );
  }
  if (b.curated_rows) {
    live.push(
      `<strong>Curated refinements</strong> — the pinned-version rows that narrow inside a reflected envelope (<code>strlen()</code> as <code>int&lt;0, max&gt;</code>).`,
    );
  }

  const off = [];
  if (b.refused_folds && b.refused_folds.length) {
    off.push(...refusedItems(b));
    off.push(
      `<strong>Integers outside ±2147483647 decline</strong> — counted through array literals, and over integer keys as well as values.`,
    );
  }
  if (b.unverified_folds && b.unverified_folds.length) {
    const names = b.unverified_folds.map((n) => `<code>${escHtml(n)}()</code>`).join(", ");
    off.push(
      `${names} <strong>do not fold either, for a different reason:</strong> their ${bitsOf(b)} behaviour is not measured, so they fold only on a provably 64-bit engine.`,
    );
  }
  if (!b.curated_rows) {
    off.push(
      `<strong>Curated refinements are off.</strong> A curated row is verified against the 64-bit engine at the pinned version, and a narrower machine at the same version can violate it — so <code>strlen()</code> is <code>int</code> here, not <code>int&lt;0, max&gt;</code>.`,
    );
  }
  if (b.fold_lane === "declined") {
    off.push(
      `<strong>Nothing folds.</strong> This engine did not report an integer width the fold lane has been verified against, and an unknown machine is not assumed.`,
    );
  }

  const framing = off.length
    ? `Everything in the second list <em>widens</em>: the analysis declines rather than guesses, so the browser is less precise than the CLI there and never wrong. Everything in the first list is a real php-src answer, not a re-implementation.`
    : `This engine answers everything the analysis knows how to ask, so nothing is held back here. Every answer above is a real php-src answer, not a re-implementation.`;
  return { live, off, framing };
}

/// The panel as one HTML fragment — what the page assigns to the panel body.
export function renderBoundaryHtml(b) {
  const c = composeBoundary(b);
  if (c === null) return null;
  const list = (items) => `<ul>${items.map((i) => `<li>${i}</li>`).join("")}</ul>`;
  return (
    `<h4>Live — answered by php-src itself</h4>${list(c.live)}` +
    (c.off.length ? `<h4>Not answered here</h4>${list(c.off)}` : "") +
    `<p class="framing">${c.framing}</p>`
  );
}
