//! The fold seam's allocation budget (issue #258): whether a call's arguments ask
//! the engine for more memory than a fold is for, priced before the call is
//! dispatched. Consulted by [`crate::fold`]'s admission sequence after the width
//! and shape gates.

use steins_sidecar::FoldArg;

/// The size an argument may ask the engine to ALLOCATE.
///
/// A fold sends the analysed source's own literals to a real PHP process, and
/// some builtins turn an integer argument into that much memory. `str_repeat`,
/// `str_pad` and `array_fill` are the obvious three, and the cost is not a slow
/// fold: past the engine's `memory_limit` PHP raises a FATAL, which is not
/// catchable, so the resident runner dies mid-NDJSON. ADR-0024's contract is
/// that a lost reply is never retried — respawn recovers the instance, not the
/// answer — so one line of analysed source (`str_repeat("ab", 2000000000)`)
/// costs a fold and prints a degradation notice for the whole run.
///
/// That is availability rather than soundness: the answer widens, it never
/// lies. But it is trivially reachable by anyone whose code Steins analyses,
/// which is the same trust boundary the callback gate is about, and refusing is
/// free — a value of a megabyte is not one this seam should be carrying into
/// the value domain anyway.
///
/// The size-shaped parameters are read from the mined `param_facts`: only the
/// declared NAME tells `str_pad($length)` from `strpos($offset)`, which is why
/// the miner keeps names. `FOLD_ALLOCATION_MAX` is a budget in the same spirit
/// as [`FOLD_ARRAY_MAX_ENTRIES`], and just as arbitrary: big enough that no
/// honest literal reaches it, small enough that the engine never notices.
///
/// The probe harness learned this the hard way and grew the same rule; the seam
/// had not, and a probe harness is the thing under our control while analysed
/// source is not.
///
/// [`FOLD_ARRAY_MAX_ENTRIES`]: crate::fold_args::FOLD_ARRAY_MAX_ENTRIES
pub(crate) fn fold_within_allocation_budget(name: &str, args: &[FoldArg]) -> bool {
    /// Bytes-ish. A `str_repeat` result of this size is already past anything a
    /// literal fold is for.
    const FOLD_ALLOCATION_MAX: i64 = 1 << 20;
    let Some(facts) = steins_catalog::param_facts(name) else {
        return true;
    };
    // The unit the count multiplies. `str_repeat("abc", n)` costs 3n bytes and
    // `str_pad("a", n)` costs n, so charging the count alone would bound the
    // wrong number: 2^20 repetitions of a 256-byte literal is 256 MB and a dead
    // child, with a count the size rule would wave through.
    let unit = match args.first() {
        Some(FoldArg::Str(subject)) => subject.len().max(1) as i64,
        _ => 1,
    };
    facts.param_names.iter().enumerate().all(|(i, pname)| {
        if !matches!(*pname, "length" | "times" | "count") {
            return true;
        }
        // A float or a numeric string reaching a size parameter coerces the same
        // way, so all three spellings are charged — and the string spelling is
        // charged through PHP's OWN numeric grammar, not Rust's.
        //
        // A first cut read the string with `parse::<i64>()` and allowed whatever
        // it could not read. PHP is weakly typed and Rust is not: `"2e9"`,
        // `" 2000000000"` and `"2000000000.0"` are all two billion to the
        // engine and all unreadable to that parser, so `str_repeat("x", "2e9")`
        // walked past the budget and killed the child — the exact bomb this
        // function exists to refuse (review finding, 2026-08-17).
        //
        // So: `php_is_numeric` decides what a number is, and a string that is
        // NOT one fails closed. At a size parameter a non-numeric string is a
        // `TypeError` under strict types and a coercion nobody should be
        // guessing at otherwise, and declining costs a fold rather than a child.
        let asked = match args.get(i) {
            Some(FoldArg::Int(v)) => *v,
            // Saturating on the cast, so `1e30` lands above the budget rather
            // than wrapping below it.
            Some(FoldArg::Float(v)) => *v as i64,
            Some(FoldArg::Str(v)) => {
                if !steins_domain::php_is_numeric(v) {
                    return false;
                }
                match v.trim().parse::<f64>() {
                    Ok(f) => f as i64,
                    // Numeric by PHP's grammar and unreadable here: refuse
                    // rather than assume it is small.
                    Err(_) => return false,
                }
            }
            _ => return true,
        };
        asked.saturating_mul(unit) <= FOLD_ALLOCATION_MAX
    })
}
