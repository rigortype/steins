//! Issue #77 — the **string-predicate transfers** at the argument-dependent rung.
//!
//! Twenty-five names whose declared return is a real `string` (`strlen`: `int`),
//! each answering one question: which [`StrPreds`] bits survive the call, and which
//! does the call establish on its own. This is the residual half of exactly the
//! names the fold lane already owns for constants — a constant subject folds one
//! rung up, and everything here is about a subject the walk knows only *predicates*
//! about.
//!
//! # Behavioral witnesses at `PINNED_PHP` (8.5.8, `php -r`)
//!
//! Every row of the table was probed rather than reasoned about, and each boundary
//! below is a decline the rule takes because a real answer contradicts the tidy
//! version of the rule:
//!
//! ```text
//! trim(' A ')                      === 'A'          (casing survives removal)
//! ltrim('0abc', '0')               === 'abc'        (a charlist changes no casing bit)
//! trim('  ')                       === ''           (…and kills the LENGTH axis)
//! substr('abc', 0, 0)              === ''           (same, for substr)
//! str_repeat('a', 0)               === ''           (non-empty must NOT transfer)
//! str_repeat('a', 2)               === 'aa'         (…and at >= 1 it must)
//! str_pad('', 1)                   === ' '          (a length >= 1 FORCES non-empty)
//! str_pad('', 0)                   === ''
//! str_pad('', -5)                  === ''
//! strtolower('ÄB')                 === 'Äb'         (THE predicate-semantics probe)
//! strtoupper('äb')                 === 'äB'
//! ucfirst('abc')                   === 'Abc'        (lowercase BREAKS — pinned decline)
//! lcfirst('ABC')                   === 'aBC'
//! ucwords('ab cd')                 === 'Ab Cd'
//! strrev('abc')                    === 'cba'
//! htmlspecialchars('<')            === '&lt;'
//! htmlspecialchars("\x80", ENT_SUBSTITUTE) === "\xEF\xBF\xBD"   (3 bytes, non-empty)
//! htmlspecialchars("\x80", ENT_QUOTES)     === ''               (the gate's reason)
//! urldecode('%30')                 === '0'          (NON_FALSY refuted; upstream keeps it)
//! escapeshellcmd("\x80")           === ''           (the whole name refused)
//! sprintf('%.0s', 'abc')           === ''           (a conversion can emit nothing)
//! sprintf('%%')                    === '%'          (…but an escape cannot)
//! sprintf('%X', 255)               === 'FF'         (no casing claim through sprintf)
//! implode(',', [])                 === ''           (no length claim through implode)
//! ```
//!
//! # Issue #41's additions, witnessed at 8.5.9
//!
//! ```text
//! substr('abc', 0)                 === 'abc'        (offset 0, no length: IDENTITY)
//! substr('a', 0, 2)                === 'a'          (a long length clamps, never pads)
//! substr('0x', 0, 1)               === '0'          (…so NON_FALSY needs length >= 2)
//! substr('abc', 0, -5)             === ''           (a negative length is not a length)
//! substr('abc', 5)                 === ''           (why every other offset declines)
//! strtr('ab', 'ab', 'xy')          === 'xy'         (byte count preserved exactly)
//! strtr('a', 'a', 'A')             === 'A'          (…so no casing bit survives)
//! strtr('a', 'ax', '0x')           === '0'          (NON_FALSY refuted; upstream keeps it)
//! strtr('a', ['a' => ''])          === ''           (an empty value DELETES)
//! strtr('a', ['a' => '0'])         === '0'          (the array form's own refutation)
//! ```
//!
//! # Issue #41's sprintf NUMERIC slice, witnessed at 8.5.9
//!
//! ```text
//! sprintf('%d', NAN)               === '0'          (int cast clamps: NUMERIC)
//! sprintf('%b', 1.0e300)           === '0'          (same clamp, any float)
//! sprintf('%o', '1e400')           === '0'          (same clamp, any string)
//! sprintf('%+d', 'not a number')   === '+0'         (int cast is total: even a
//!                                                     non-numeric string is safe)
//! sprintf('%f', NAN)               === 'NaN'        (float FORMAT does not clamp:
//!                                                     NOT a numeric string)
//! sprintf('%f', INF)               === 'INF'        (same, for +/-infinity)
//! sprintf('%f', '1e400')           === 'INF'        ((float) cast can overflow a
//!                                                     numeric STRING argument too)
//! sprintf('%14x', 255)             === 'ff'         (the excluded hex pair —
//! sprintf('%14X', 255)             === 'FF'          upstream claims both numeric)
//! ```
//!
//! `strtolower('ÄB') === 'Äb'` is the probe the whole casing half rests on. Steins'
//! `LOWERCASE` is `php_str_is_lowercase`, i.e. **no ASCII uppercase byte** — which
//! `'Äb'` satisfies — so `strtolower` establishes the predicate for *any* input,
//! not only an ASCII one. Had the predicate meant "every character is lowercase in
//! Unicode's sense", the forced leg would have needed a non-ASCII-excluding subject
//! fact; it does not, and this file pins that reading from both sides.
//!
//! Zero emission is asserted on every fixture: a transfer is a *type*, never a
//! finding.

use std::collections::HashMap;

use steins_domain::{Base, Fact, IntRange, PhpStr, Refinement, StrPreds, Val};
use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// The family's reflected declaration at `PINNED_PHP` — a real pin for every name
/// (`ReflectionFunction::getReturnType`, non-tentative, verbatim).
const FAMILY: &[(&str, &str)] = &[
    ("trim", "string"),
    ("ltrim", "string"),
    ("rtrim", "string"),
    ("chop", "string"),
    ("substr", "string"),
    ("strrev", "string"),
    ("strtr", "string"),
    ("str_repeat", "string"),
    ("str_pad", "string"),
    ("strtolower", "string"),
    ("strtoupper", "string"),
    ("ucfirst", "string"),
    ("lcfirst", "string"),
    ("ucwords", "string"),
    ("implode", "string"),
    ("join", "string"),
    ("addslashes", "string"),
    ("addcslashes", "string"),
    ("escapeshellarg", "string"),
    ("escapeshellcmd", "string"),
    ("urlencode", "string"),
    ("urldecode", "string"),
    ("rawurlencode", "string"),
    ("rawurldecode", "string"),
    ("preg_quote", "string"),
    ("htmlspecialchars", "string"),
    ("htmlentities", "string"),
    ("sprintf", "string"),
    ("vsprintf", "string"),
    ("strlen", "int"),
];

/// A mock PHP answering the one reflection surface this rung's gate consults — the
/// declaration — plus the reflected envelope the ladder falls back to when a
/// transfer declines, so a decline is visible as `string` rather than `unknown`.
struct Mock {
    types: HashMap<String, String>,
    facts: HashMap<String, Fact>,
}

impl Mock {
    /// The pinned engine: every family member declared as it really is.
    fn sidecar() -> Mock {
        let mut types = HashMap::new();
        let mut facts = HashMap::new();
        for (name, declared) in FAMILY {
            types.insert((*name).to_owned(), (*declared).to_owned());
            facts.insert(
                (*name).to_owned(),
                if *declared == "int" {
                    Fact::General { base: Base::Int, nullable: false }
                } else {
                    Fact::General { base: Base::String, nullable: false }
                },
            );
        }
        Mock { types, facts }
    }

    /// The same engine whose declaration for `name` has MOVED — php-src grew an arm
    /// the rule predates. The rule must go quiet, not adapt.
    fn with_declaration(name: &str, declared: &str) -> Mock {
        let mut m = Mock::sidecar();
        m.types.insert(name.to_owned(), declared.to_owned());
        m
    }

    /// An engine that reflects nothing at all for `name` — no PHP, or a name this
    /// runtime has never heard of.
    fn silent_about(name: &str) -> Mock {
        let mut m = Mock::sidecar();
        m.types.remove(name);
        m.facts.remove(name);
        m
    }
}

impl Folder for Mock {
    fn fold(&mut self, _name: &str, _args: &[ArgValue]) -> Option<ArgValue> {
        None
    }
    fn absence_family_available(&mut self) -> bool {
        true
    }
    fn builtin_return_fact(&mut self, name: &str) -> Option<Fact> {
        self.facts.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
}

/// The single `debug.type` body a one-dump source produces, asserting on the way
/// that the source produced NO other finding.
fn one_type_with(src: &str, folder: &mut dyn Folder) -> String {
    let tree = SourceTree::parse(src);
    let ds = check_with(&tree, &[], "t.php", folder);
    // `untyped.*` is contract-layer claim-absence (issue #200), orthogonal to the
    // transfer semantics this harness asserts (its fixtures deliberately carry
    // untyped-but-bound parameters so `variable.undefined` stays out of frame).
    let other: Vec<&Diagnostic> =
        ds.iter().filter(|d| !d.id.starts_with("debug.") && !d.id.starts_with("untyped.")).collect();
    assert!(other.is_empty(), "a string-predicate transfer emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A string-declared fixture: `@param <decl> $v`, one dump of `<expr>`.
///
/// `$n` and `$p` are untyped parameters the fixtures use wherever an argument must
/// be an *unknown* int or string: untyped means no fact, which is the premise under
/// test, and being parameters means they are bound (so `variable.undefined` — which
/// this helper's no-other-finding assertion would otherwise catch — has nothing to
/// say about them).
fn dump_with(decl: &str, expr: &str, folder: &mut dyn Folder) -> String {
    one_type_with(
        &format!(
            "<?php\n/** @param {decl} $v */\nfunction f(string $v, $n, $p): void {{ \\PHPStan\\dumpType({expr}); }}\n"
        ),
        folder,
    )
}

fn dump(decl: &str, expr: &str) -> String {
    dump_with(decl, expr, &mut Mock::sidecar())
}

// Casing survives removal, permutation and repetition

#[test]
fn the_trim_family_carries_casing_through_any_charlist() {
    // `trim(' A ') === 'A'`: whatever is removed, no ASCII case byte is ADDED.
    for f in ["trim", "ltrim", "rtrim", "chop"] {
        assert_eq!(
            dump("uppercase-string", &format!("{f}($v)")),
            "dumped type: uppercase-string (asserted)",
            "{f} keeps uppercase-ness"
        );
        assert_eq!(
            dump("lowercase-string", &format!("{f}($v)")),
            "dumped type: lowercase-string (asserted)",
            "{f} keeps lowercase-ness"
        );
        // `ltrim('0abc', '0') === 'abc'` — an explicit charlist can strip anything,
        // and casing is unmoved by that because the output is still a substring.
        assert_eq!(
            dump("lowercase-string", &format!("{f}($v, '0')")),
            "dumped type: lowercase-string (asserted)",
            "{f} keeps casing under an explicit charlist"
        );
        // …and a NON-CONSTANT charlist is equally harmless, for the same reason.
        assert_eq!(
            dump("lowercase-string", &format!("{f}($v, $v)")),
            "dumped type: lowercase-string (asserted)"
        );
    }
}

#[test]
fn the_trim_family_never_carries_the_length_axis() {
    // `trim('  ') === ''`: an all-whitespace non-empty subject trims to empty, so
    // the length axis cannot survive — the rule DECLINES rather than transferring,
    // and the reflected `string` envelope stands.
    for f in ["trim", "ltrim", "rtrim", "chop"] {
        assert_eq!(dump("non-empty-string", &format!("{f}($v)")), "dumped type: string");
        assert_eq!(dump("non-falsy-string", &format!("{f}($v)")), "dumped type: string");
    }
}

#[test]
fn substr_carries_casing_from_every_window() {
    // `substr($lowercase, 0, 5)` is a substring: casing holds wherever the window
    // sits, including the offsets the length axis refuses.
    assert_eq!(dump("lowercase-string", "substr($v, 5)"), "dumped type: lowercase-string (asserted)");
    assert_eq!(dump("uppercase-string", "substr($v, -5)"), "dumped type: uppercase-string (asserted)");
    assert_eq!(
        dump("lowercase-string", "substr($v, 0, 5)"),
        "dumped type: lowercase-string (asserted)"
    );
}

/// Issue #41 — the length axis, which lives entirely at a **provably zero** offset.
#[test]
fn substr_carries_the_length_axis_only_from_a_zero_offset() {
    // `substr('abc', 0) === 'abc'` — the identity, so the whole axis rides along.
    assert_eq!(dump("non-empty-string", "substr($v, 0)"), "dumped type: non-empty-string (asserted)");
    assert_eq!(dump("non-falsy-string", "substr($v, 0)"), "dumped type: non-falsy-string (asserted)");
    // `substr('a', 0, 2) === 'a'`: a length past the end clamps, never pads.
    assert_eq!(
        dump("non-empty-string", "substr($v, 0, 1)"),
        "dumped type: non-empty-string (asserted)"
    );
    // At a length >= 2 the output is two bytes or the whole subject — either way
    // not `'0'` — so non-falsiness survives too (`substr('0x', 0, 2) === '0x'`).
    assert_eq!(
        dump("non-falsy-string", "substr($v, 0, 2)"),
        "dumped type: non-falsy-string (asserted)"
    );
    // …but at a length of exactly 1 a non-falsy subject can answer `'0'`
    // (`substr('0x', 0, 1) === '0'`), so only non-emptiness survives there.
    assert_eq!(
        dump("non-falsy-string", "substr($v, 0, 1)"),
        "dumped type: non-empty-string (asserted)"
    );
    // `substr('abc', 0, 0) === ''` and `substr('abc', 0, -5) === ''`.
    assert_eq!(dump("non-empty-string", "substr($v, 0, 0)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "substr($v, 0, -5)"), "dumped type: string");
    // An unseen length, and an unseen or non-zero offset, all decline the axis:
    // `substr('abc', 5) === ''` and this rung does not carry `strlen($s)`.
    assert_eq!(dump("non-empty-string", "substr($v, 0, $n)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "substr($v, 1, 1)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "substr($v, $n, 1)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "substr($v, 5)"), "dumped type: string");
}

/// Issue #41 — `strtr` preserves the byte COUNT (3-arg) or replaces substrings by
/// non-empty ones (2-arg); either way non-emptiness survives and nothing else does.
#[test]
fn strtr_carries_non_emptiness_and_refuses_everything_else() {
    // `strtr('ab', 'ab', 'xy') === 'xy'` — one byte in, one byte out.
    assert_eq!(
        dump("non-empty-string", "strtr($v, $p, $p)"),
        "dumped type: non-empty-string (asserted)"
    );
    // THE refusal, against upstream's own claim: `strtr('a', 'ax', '0x') === '0'`,
    // so a non-falsy subject does NOT stay non-falsy — it degrades to non-empty.
    assert_eq!(
        dump("non-falsy-string", "strtr($v, 'ax', '0x')"),
        "dumped type: non-empty-string (asserted)"
    );
    // `strtr('a', 'a', 'A') === 'A'` — the target byte is arbitrary, so casing goes.
    assert_eq!(dump("lowercase-string", "strtr($v, $p, $p)"), "dumped type: string");
    assert_eq!(dump("uppercase-string", "strtr($v, $p, $p)"), "dumped type: string");
    // A subject with no length claim gets nothing: the rule transfers, never forces.
    assert_eq!(dump("string", "strtr($v, $p, $p)"), "dumped type: string");
}

#[test]
fn the_strtr_array_form_reads_its_replacement_values() {
    // Every value non-empty ⇒ non-emptiness survives (`strtr('abc', ['ab' => 'Z'])`).
    assert_eq!(
        dump("non-empty-string", "strtr($v, ['a' => 'x', 'b' => 'yy'])"),
        "dumped type: non-empty-string (asserted)"
    );
    // One empty value DELETES: `strtr('a', ['a' => '']) === ''`.
    assert_eq!(dump("non-empty-string", "strtr($v, ['a' => 'x', 'b' => ''])"), "dumped type: string");
    // `strtr('a', ['a' => '0']) === '0'` — the same refusal the 3-arg form takes.
    assert_eq!(
        dump("non-falsy-string", "strtr($v, ['a' => '0'])"),
        "dumped type: non-empty-string (asserted)"
    );
    // An array this rung cannot see through declines rather than guessing.
    assert_eq!(dump("non-empty-string", "strtr($v, $p)"), "dumped type: string");
    // A fourth argument is not this function's signature.
    assert_eq!(dump("non-empty-string", "strtr($v, $p, $p, $p)"), "dumped type: string");
}

#[test]
fn strrev_preserves_the_whole_byte_multiset() {
    // The length is exactly preserved, so `''`/`'0'` can only come from `''`/`'0'`.
    assert_eq!(dump("non-empty-string", "strrev($v)"), "dumped type: non-empty-string (asserted)");
    assert_eq!(dump("non-falsy-string", "strrev($v)"), "dumped type: non-falsy-string (asserted)");
    assert_eq!(dump("lowercase-string", "strrev($v)"), "dumped type: lowercase-string (asserted)");
    assert_eq!(dump("uppercase-string", "strrev($v)"), "dumped type: uppercase-string (asserted)");
}

// `str_repeat`: the multiplier is the whole gate on the length axis

#[test]
fn str_repeat_transfers_non_emptiness_only_at_a_multiplier_of_at_least_one() {
    // `str_repeat($ne, 2)` is non-empty…
    assert_eq!(
        dump("non-empty-string", "str_repeat($v, 2)"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(
        dump("non-falsy-string", "str_repeat($v, 1)"),
        "dumped type: non-falsy-string (asserted)"
    );
    // …and `str_repeat('a', 0) === ''` is why the SAME subject at zero declines.
    assert_eq!(dump("non-empty-string", "str_repeat($v, 0)"), "dumped type: string");
    // A multiplier the rule cannot see through declines the length axis too.
    assert_eq!(dump("non-empty-string", "str_repeat($v, $n)"), "dumped type: string");
}

#[test]
fn str_repeat_carries_casing_at_every_multiplier() {
    // `''` carries both casing bits, so the zero case cannot falsify a casing claim
    // and the gate above does not apply to this axis.
    assert_eq!(
        dump("lowercase-string", "str_repeat($v, 5)"),
        "dumped type: lowercase-string (asserted)"
    );
    assert_eq!(dump("lowercase-string", "str_repeat($v, 0)"), "dumped type: lowercase-string (asserted)");
    assert_eq!(
        dump("uppercase-string", "str_repeat($v, $n)"),
        "dumped type: uppercase-string (asserted)"
    );
}

// `str_pad`: the subject is a subsequence, and the length can FORCE non-emptiness

#[test]
fn str_pad_forces_non_emptiness_at_a_length_of_at_least_one() {
    // `str_pad('', 1) === ' '` — true for ANY subject, including one with no fact.
    assert_eq!(dump("string", "str_pad($v, 5)"), "dumped type: non-empty-string");
    // `str_pad('', 0) === ''` and `str_pad('', -5) === ''` — no force below 1.
    assert_eq!(dump("string", "str_pad($v, 0)"), "dumped type: string");
    assert_eq!(dump("string", "str_pad($v, -5)"), "dumped type: string");
    assert_eq!(dump("string", "str_pad($v, $n)"), "dumped type: string");
    // The subject's own length axis survives regardless of the length argument —
    // `str_pad` never shortens.
    assert_eq!(dump("non-empty-string", "str_pad($v, 0)"), "dumped type: non-empty-string (asserted)");
    assert_eq!(dump("non-falsy-string", "str_pad($v, 0)"), "dumped type: non-falsy-string (asserted)");
}

#[test]
fn str_pad_carries_casing_only_when_the_pad_string_carries_it_too() {
    // The default pad is `' '`, which has no cased character.
    assert_eq!(
        dump("lowercase-string", "str_pad($v, 5)"),
        "dumped type: non-empty-lowercase-string (asserted)"
    );
    // A constant pad is checked directly.
    assert_eq!(
        dump("lowercase-string", "str_pad($v, 5, '-')"),
        "dumped type: non-empty-lowercase-string (asserted)"
    );
    // An UPPERCASE pad breaks a lowercase subject — the padding is inserted verbatim.
    assert_eq!(dump("lowercase-string", "str_pad($v, 5, 'X')"), "dumped type: non-empty-string (asserted)");
    // …and an unknown pad drops the casing half while keeping the forced length.
    assert_eq!(dump("lowercase-string", "str_pad($v, 5, $p)"), "dumped type: non-empty-string (asserted)");
}

// The forced casing pair — the predicate-semantics probe, from both sides

#[test]
fn strtolower_and_strtoupper_force_their_casing_for_any_subject() {
    // `strtolower('ÄB') === 'Äb'`: the non-ASCII byte is untouched and the result
    // still has NO ASCII uppercase byte, which IS Steins' `LOWERCASE`. So the claim
    // holds for an arbitrary subject — no fact required, and none consulted.
    assert_eq!(dump("string", "strtolower($v)"), "dumped type: lowercase-string");
    assert_eq!(dump("string", "strtoupper($v)"), "dumped type: uppercase-string");
    // The length axis rides along (byte-wise mapping preserves the length).
    assert_eq!(
        dump("non-empty-string", "strtolower($v)"),
        "dumped type: non-empty-lowercase-string (asserted)"
    );
    // The OPPOSITE casing is dropped: `strtolower('AB') === 'ab'` has lowercase
    // bytes, so an uppercase subject does not survive as one.
    assert_eq!(dump("uppercase-string", "strtolower($v)"), "dumped type: lowercase-string (asserted)");
    assert_eq!(dump("lowercase-string", "strtoupper($v)"), "dumped type: uppercase-string (asserted)");
    // A second argument is not this function's signature — decline.
    assert_eq!(dump("string", "strtolower($v, 1)"), "dumped type: string");
}

// The selective-casing trio: what survives, and the pinned DECLINE

#[test]
fn ucfirst_breaks_lowercase_and_lcfirst_breaks_uppercase() {
    // THE pinned decline: `ucfirst('abc') === 'Abc'`, so a lowercase subject does
    // not stay lowercase and the rule says nothing.
    assert_eq!(dump("lowercase-string", "ucfirst($v)"), "dumped type: string");
    assert_eq!(dump("lowercase-string", "ucwords($v)"), "dumped type: string");
    assert_eq!(dump("uppercase-string", "lcfirst($v)"), "dumped type: string");
    // …and the mirror survives, because each function only moves case ONE way.
    assert_eq!(dump("uppercase-string", "ucfirst($v)"), "dumped type: uppercase-string (asserted)");
    assert_eq!(dump("uppercase-string", "ucwords($v)"), "dumped type: uppercase-string (asserted)");
    assert_eq!(dump("lowercase-string", "lcfirst($v)"), "dumped type: lowercase-string (asserted)");
}

#[test]
fn the_selective_casing_trio_carries_the_length_axis() {
    for f in ["ucfirst", "lcfirst", "ucwords"] {
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v)")),
            "dumped type: non-empty-string (asserted)",
            "{f} keeps non-emptiness"
        );
        assert_eq!(
            dump("non-falsy-string", &format!("{f}($v)")),
            "dumped type: non-falsy-string (asserted)",
            "{f} keeps non-falsiness"
        );
    }
    // `ucwords($s, $separators)` is the same transfer.
    assert_eq!(dump("non-empty-string", "ucwords($v, '-')"), "dumped type: non-empty-string (asserted)");
}

// The escaping family, and the two places upstream is WRONG at this engine

#[test]
fn the_escaping_family_carries_the_length_axis() {
    for f in [
        "addslashes",
        "escapeshellarg",
        "urlencode",
        "rawurlencode",
        "preg_quote",
        "htmlspecialchars",
        "htmlentities",
    ] {
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v)")),
            "dumped type: non-empty-string (asserted)",
            "{f} keeps non-emptiness"
        );
        assert_eq!(
            dump("non-falsy-string", &format!("{f}($v)")),
            "dumped type: non-falsy-string (asserted)",
            "{f} keeps non-falsiness"
        );
        // Casing is NOT claimed: `htmlspecialchars('<') === '&lt;'` introduces
        // lowercase letters and `urlencode('ä') === '%C3%A4'` uppercase hex.
        assert_eq!(
            dump("lowercase-string", &format!("{f}($v)")),
            "dumped type: string",
            "{f} makes no casing claim"
        );
    }
    // `addcslashes` takes its character list as a REQUIRED second argument.
    assert_eq!(
        dump("non-empty-string", "addcslashes($v, 'a')"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(dump("non-empty-string", "addcslashes($v)"), "dumped type: string");
}

#[test]
fn urldecode_keeps_non_emptiness_but_not_non_falsiness() {
    // `urldecode('%30') === '0'`: decoding SHRINKS, so a non-falsy subject can
    // decode to the one falsy string of length 1. Upstream PHPStan propagates
    // non-falsiness through these two; the probe refutes it, and the measured
    // counterexample wins (ADR-0061 §2).
    for f in ["urldecode", "rawurldecode"] {
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v)")),
            "dumped type: non-empty-string (asserted)",
            "{f} keeps non-emptiness"
        );
        assert_eq!(
            dump("non-falsy-string", &format!("{f}($v)")),
            "dumped type: non-empty-string (asserted)",
            "{f} widens non-falsy to non-empty"
        );
    }
}

#[test]
fn escapeshellcmd_is_refused_outright() {
    // Upstream PHPStan lists it with the other ten. At 8.5.8
    // `escapeshellcmd("\x80") === ''` — it DROPS an invalid multibyte sequence, so
    // a non-empty subject can produce the empty string. The name gets no rule.
    assert_eq!(dump("non-empty-string", "escapeshellcmd($v)"), "dumped type: string");
    assert_eq!(dump("non-falsy-string", "escapeshellcmd($v)"), "dumped type: string");
}

#[test]
fn htmlspecialchars_needs_the_substitute_flag() {
    // The 8.1+ default flags are `ENT_QUOTES | ENT_SUBSTITUTE | ENT_HTML401` (11),
    // so an ABSENT flags argument carries the bit.
    for f in ["htmlspecialchars", "htmlentities"] {
        // `ENT_SUBSTITUTE` is 8; `ENT_QUOTES | ENT_SUBSTITUTE | ENT_HTML401` is 11.
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v, 8)")),
            "dumped type: non-empty-string (asserted)",
            "{f} under an explicit substitute bit"
        );
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v, 11)")),
            "dumped type: non-empty-string (asserted)",
            "{f} under the 8.1+ default flag word"
        );
        // A *named* global constant does not lower into the trace IR at all
        // (`ArgValue` has no global-constant form), so `ENT_SUBSTITUTE` spelled by
        // name reads as an unknown flags argument and declines — conservative, and
        // recorded here so the boundary is not mistaken for a bug in the gate.
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v, ENT_SUBSTITUTE)")),
            "dumped type: string",
            "{f} under a named constant"
        );
        // A flags value WITHOUT the bit is the empty-out path:
        // `htmlspecialchars("\x80", ENT_QUOTES) === ''`.
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v, 0)")),
            "dumped type: string",
            "{f} without the substitute bit"
        );
        // …and a flags argument the rule cannot see through declines the same way.
        // The unknown flag is `$n`, the helper's *bound* untyped parameter: issue
        // #41 certified `htmlspecialchars`' arguments as by-value, which resolves
        // the callee at this site and lets `variable.undefined` see an unbound name
        // it previously could not — a correct finding, and this fixture is about the
        // transfer, not about it.
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v, $n)")),
            "dumped type: string",
            "{f} under a non-constant flags argument"
        );
    }
}

// `implode`: every contributor must carry the claim

#[test]
fn implode_carries_casing_from_the_glue_and_every_element() {
    let src = |decl: &str, expr: &str| {
        format!(
            "<?php\n/** @param lowercase-string $g\n * @param {decl} $a */\nfunction f(string $g, array $a): void {{ \\PHPStan\\dumpType({expr}); }}\n"
        )
    };
    // Glue and elements both lowercase ⇒ the concatenation is lowercase.
    assert_eq!(
        one_type_with(&src("array<lowercase-string>", "implode($g, $a)"), &mut Mock::sidecar()),
        "dumped type: lowercase-string (asserted)"
    );
    // A plain-string glue contributes nothing, so the claim dies at the glue.
    assert_eq!(
        one_type_with(
            "<?php\n/** @param array<lowercase-string> $a */\nfunction f(string $g, array $a): void { \\PHPStan\\dumpType(implode($g, $a)); }\n",
            &mut Mock::sidecar()
        ),
        "dumped type: string"
    );
    // …and plain-string elements kill it from the other side.
    assert_eq!(
        one_type_with(&src("array<string>", "implode($g, $a)"), &mut Mock::sidecar()),
        "dumped type: string"
    );
    // The one-argument form's glue is `''`, which carries both casing bits.
    assert_eq!(
        one_type_with(&src("array<uppercase-string>", "implode($a)"), &mut Mock::sidecar()),
        "dumped type: uppercase-string (asserted)"
    );
    // `join` is the same function under its other name.
    assert_eq!(
        one_type_with(&src("array<lowercase-string>", "join($g, $a)"), &mut Mock::sidecar()),
        "dumped type: lowercase-string (asserted)"
    );
}

#[test]
fn implode_never_claims_the_length_axis() {
    // `implode(',', []) === ''`: a non-empty element proves nothing about the
    // result while the array itself may be empty.
    assert_eq!(
        one_type_with(
            "<?php\n/** @param non-empty-array<non-empty-string> $a */\nfunction f(array $a): void { \\PHPStan\\dumpType(implode(',', $a)); }\n",
            &mut Mock::sidecar()
        ),
        "dumped type: string"
    );
}

// `sprintf`: a literal byte in a constant format, and nothing else

#[test]
fn sprintf_claims_non_emptiness_only_from_a_literal_byte() {
    // `sprintf('%s0%s', $s, $s)` always emits the literal `'0'`.
    assert_eq!(
        dump("string", "sprintf('%s0%s', $v, $v)"),
        "dumped type: non-empty-string"
    );
    assert_eq!(dump("string", "sprintf('Hello %s', $v)"), "dumped type: non-empty-string");
    // `sprintf('%%')` emits one `'%'`.
    assert_eq!(dump("string", "sprintf('%%%s', $v)"), "dumped type: non-empty-string");
    // All-conversion formats claim nothing: `sprintf('%.0s', 'abc') === ''`.
    assert_eq!(dump("string", "sprintf('%s', $v)"), "dumped type: string");
    assert_eq!(dump("string", "sprintf('%.0s', $v)"), "dumped type: string");
    assert_eq!(dump("string", "sprintf(\"%'x5s\", $v)"), "dumped type: string");
    // A non-constant format declines outright — v1 reads no predicates off it.
    assert_eq!(dump("non-empty-string", "sprintf($v, $v)"), "dumped type: string");
    // No casing claim: `sprintf('%X', 255) === 'FF'`.
    assert_eq!(dump("lowercase-string", "sprintf('a%s', $v)"), "dumped type: non-empty-string (asserted)");
}

#[test]
fn the_sprintf_format_scanner_matches_the_engine_on_every_probed_shape() {
    // The scanner was differentialled against 8.5.8 over the whole documented
    // `%[argnum$][flags][width][.precision]specifier` grammar, calling each format
    // with as many EMPTY-STRING arguments as it needs — the worst case for a
    // non-emptiness claim. Every claim held and nothing was mis-parsed:
    //
    //   claimed non-empty : 'a'  'abc'  '%%'  '100%%'  '%s0%s' → '0'
    //                       'Hello %s' → 'Hello '  'x%sy' → 'xy'
    //                       '%s %s' → ' '  '%2$s %1$s' → ' '  '%%%s' → '%'  '%s%%' → '%'
    //   claimed nothing   : '%s' → ''  '%.0s' → ''  '%1$s' → ''  '%c' → "\0"
    //                       '%x' '%X' '%u' '%g' '%G' → '0'
    //                       '%-5s' → '     '  "%'x5s" → 'xxxxx'
    //                       '%10.3f' → '     0.000'
    //                       "%'-10s" → '----------'  "%-'x10s"/"%1$'x10s" → 'xxxxxxxxxx'
    //   DECLINED (parse)  : '%'  '%s%'  → ValueError "Missing format specifier at end
    //                       of string";  '%z' → ValueError 'Unknown format specifier "z"'
    //
    // `'%d'`/`'%b'`/`'%o'`/`'%05d'`/`'%+d'` moved OUT of the "claims nothing" bucket
    // in issue #41: each is an int-cast whole-format conversion, and the int cast
    // never renders a non-digit byte for ANY input — `%d`/`%b`/`%o` claim NUMERIC
    // even from a plain `string` value (`php -r`-witnessed: `sprintf('%+d', "not a
    // number") === '+0'`). `%e`/`%f`/`%g` are the FLOAT-format trio and stay in the
    // "claims nothing" bucket here on purpose: they need a proven `int` value
    // before NUMERIC is safe (a `NAN`/`INF` float, or an overflowing numeric
    // string, renders as non-numeric text) — see
    // `sprintf_gates_the_float_trio_on_a_proven_int_value` below, which is the test
    // that exercises them against an `int`-typed value instead of this `string`
    // one.
    //
    // Representative cases from that table.
    for claims in ["'abc'", "'100%%'", "'x%sy'", "'%s %s'", "'%2$s %1$s'", "'%s%%'"] {
        assert_eq!(
            dump("string", &format!("sprintf({claims}, $v, $v)")),
            "dumped type: non-empty-string",
            "{claims} emits a literal byte"
        );
    }
    for silent in ["'%s'", "'%.0s'", "'%1$s'", "'%c'", "'%X'", "'%-5s'", "'%10.3f'"] {
        assert_eq!(
            dump("string", &format!("sprintf({silent}, $v)")),
            "dumped type: string",
            "{silent} can emit nothing"
        );
    }
    // The int-cast trio claims NUMERIC even from a plain `string` value.
    for numeric in ["'%d'", "'%b'", "'%o'", "'%05d'", "'%+d'"] {
        assert_eq!(
            dump("string", &format!("sprintf({numeric}, $v)")),
            "dumped type: numeric-string",
            "{numeric} is an unconditional int-cast conversion"
        );
    }
    // A format the engine itself refuses is refused here: no return value exists
    // for the rule to describe, and a mis-parse would be a false premise.
    for refused in ["'%'", "'%s%'", "'%z'"] {
        assert_eq!(
            dump("string", &format!("sprintf({refused}, $v)")),
            "dumped type: string",
            "{refused} is a ValueError at 8.5.8"
        );
    }
}

/// A helper mirroring `dump`, but the tested variable is a NATIVE `int` parameter
/// rather than `dump`'s native `string` one. Issue #41's float-trio gate needs a
/// genuinely `int`-typed value to admit, and a `@param int $v` docblock cannot
/// narrow `dump`'s native `string` parameter to `int` — that would be a
/// base-type mismatch the docblock/native reconciliation itself would flag,
/// tripping `one_type_with`'s "no other finding" assertion.
fn dump_int(expr: &str) -> String {
    one_type_with(
        &format!("<?php\nfunction f(int $v): void {{ \\PHPStan\\dumpType({expr}); }}\n"),
        &mut Mock::sidecar(),
    )
}

/// Issue #41 — the int-cast trio (`%b`/`%d`/`%o`) forces `NUMERIC` unconditionally,
/// exactly as [`the_sprintf_format_scanner_matches_the_engine_on_every_probed_shape`]
/// already showed from a plain `string` value; this pins the same claim from a
/// proven `int` one, and adds the width/sign/precision-adjacent shapes that value
/// type does not affect (`%b`/`%o` have no precision, so only `%d` needs one).
#[test]
fn sprintf_admits_numeric_from_the_int_cast_trio_from_any_value_type() {
    for f in ["'%b'", "'%d'", "'%o'", "'%05d'", "'%+d'", "'%-10d'", "'% d'"] {
        assert_eq!(
            dump_int(&format!("sprintf({f}, $v)")),
            "dumped type: numeric-string",
            "{f} is an unconditional int-cast conversion"
        );
    }
}

/// Issue #41 — the float-format trio (`%e`/`%f`/`%g`) forces `NUMERIC` only when
/// the paired value argument is provably `int`. A `string` value declines even
/// though this scanner cannot see through it, because PHP's float formatter
/// renders a `NAN`/`INF`/`-INF` value verbatim (`sprintf('%f', NAN) === 'NaN'`)
/// and a numeric STRING can overflow its own `(float)` cast to `INF`
/// (`sprintf('%f', "1e400") === 'INF'`, both `php -r`-witnessed at 8.5.9) — a
/// native `int` cannot hold either special value, closing both holes at once.
#[test]
fn sprintf_gates_the_float_trio_on_a_proven_int_value() {
    for f in ["'%e'", "'%f'", "'%g'", "'%14e'", "'%.2f'", "'%05.2f'"] {
        assert_eq!(
            dump_int(&format!("sprintf({f}, $v)")),
            "dumped type: numeric-string",
            "{f} of a proven int value is NUMERIC"
        );
        assert_eq!(
            dump("string", &format!("sprintf({f}, $v)")),
            "dumped type: string",
            "{f} of an unproven string value declines"
        );
    }
}

/// Issue #41 — the excluded hex pair. `%x`/`%X` are int-cast conversions exactly
/// like `%d`/`%b`/`%o`, but their digits are hexadecimal (`sprintf('%14x', 255)
/// === 'ff'`), and a hex string is never a PHP numeric string — refused
/// regardless of the value's type, pinned against upstream PHPStan's own
/// `bug-7387.php` fixture, which claims `numeric-string` for `%14X` (and, per
/// this issue's tracking comment, `%14x`) and is wrong about both at this pin.
#[test]
fn sprintf_declines_the_hex_pair_even_from_a_proven_int_value() {
    for f in ["'%x'", "'%X'", "'%14x'", "'%14X'"] {
        assert_eq!(
            dump_int(&format!("sprintf({f}, $v)")),
            "dumped type: string",
            "{f} is hexadecimal, never a numeric string"
        );
    }
}

/// Issue #41 — the "WHOLE format" requirement. `%d`/`%b`/`%o` alone would force
/// NUMERIC, but a literal byte anywhere, a second specifier, a custom pad, or an
/// explicit position all decline it — declining is silence, never a lie.
#[test]
fn sprintf_declines_numeric_when_the_format_is_not_purely_the_conversion() {
    // Literal text around the conversion still earns `NON_EMPTY` from the
    // existing literal-byte rule (unaffected by this issue), but never `NUMERIC`
    // — the claim is about the WHOLE format, and one literal byte breaks it.
    // `'%%d'` is the escape case: `%%` is a guaranteed literal `'%'`, and the
    // trailing `d` is a second literal byte, not a specifier at all.
    for f in ["'literal %d'", "'%d literal'", "' %d'", "'%d %d'", "'%%d'"] {
        assert_eq!(
            dump_int(&format!("sprintf({f}, $v, $v)")),
            "dumped type: non-empty-string",
            "{f} carries a literal byte, so NON_EMPTY holds but not NUMERIC"
        );
    }
    // All-conversion shapes that are still not ONE admitted conversion: no
    // literal byte survives either scanner, so both legs decline outright.
    for f in ["\"%'*10d\"", "'%1$d'", "'%d%d'"] {
        assert_eq!(
            dump_int(&format!("sprintf({f}, $v, $v)")),
            "dumped type: string",
            "{f} is neither a literal byte nor a single admitted conversion"
        );
    }
}

/// Issue #41 — `vsprintf` reads the same whole-format scanner as `sprintf`, but
/// only ever admits the int-cast trio: PHPStan's own `bug-7387.php` fixture
/// (`vsprintf("%4d", $array)` against a plain, UNSHAPED `array` parameter) is
/// exactly the case this leg is for — `%b`/`%d`/`%o` need no look inside the
/// values array at all, so an array this rule cannot see through still admits.
/// The float trio and the hex pair are pinned as continuing declines: this rule
/// never opens `vsprintf`'s values array to find the one value being converted,
/// so `%e`/`%f`/`%g` cannot clear their `int`-value gate for `vsprintf` the way
/// they do for `sprintf`, and `%x`/`%X` are refused for the same hex reason
/// either name takes.
#[test]
fn vsprintf_admits_numeric_from_the_int_cast_trio_but_not_the_float_trio_or_the_hex_pair() {
    let src = |f: &str| {
        format!(
            "<?php\nfunction f(array $arr): void {{ \\PHPStan\\dumpType(vsprintf({f}, $arr)); }}\n"
        )
    };
    for f in ["'%b'", "'%d'", "'%o'", "'%05d'"] {
        assert_eq!(
            one_type_with(&src(f), &mut Mock::sidecar()),
            "dumped type: numeric-string",
            "vsprintf({f}, ...) is an unconditional int-cast conversion"
        );
    }
    // The float trio and the hex pair decline for `vsprintf`.
    for f in ["'%e'", "'%f'", "'%g'", "'%x'", "'%X'"] {
        assert_eq!(
            one_type_with(&src(f), &mut Mock::sidecar()),
            "dumped type: string",
            "vsprintf({f}, ...) does not admit NUMERIC"
        );
    }
    // A literal byte still earns NON_EMPTY, exactly as it does for `sprintf`.
    assert_eq!(
        one_type_with(&src("'literal %d'"), &mut Mock::sidecar()),
        "dumped type: non-empty-string"
    );
}

/// Issue #41's precedence question, pinned rather than left to registration order:
/// where an argument-dependent rule and an **admitted curated static row** describe
/// the same call, the rule wins — and it can only win by narrowing strictly inside
/// the row, which is what makes the precedence safe instead of a coin toss.
///
/// `strlen` is the family's one such collision. The fixture hands the walk what the
/// real pipeline hands it once ADR-0056's gate has admitted `("strlen",
/// "int<0, max>")`: the curated row itself, not the bare `int` envelope.
#[test]
fn an_argument_dependent_rule_outranks_the_admitted_curated_row() {
    let curated = || {
        let mut m = Mock::sidecar();
        m.facts.insert(
            "strlen".to_owned(),
            Fact::refined(Base::Int, Refinement::Int(IntRange::NON_NEGATIVE), false),
        );
        m
    };
    // The rule fires and REPLACES the row, because `int<1, max>` is strictly
    // inside `int<0, max>`.
    assert_eq!(
        dump_with("non-empty-string", "strlen($v)", &mut curated()),
        "dumped type: int<1, max> (asserted)"
    );
    // Where the rule declines, the curated row is untouched — the precedence is a
    // replacement by a narrower answer, never a suppression of the row.
    assert_eq!(dump_with("string", "strlen($v)", &mut curated()), "dumped type: int<0, max>");
}

// `strlen`: the one member that answers an int

#[test]
fn strlen_of_a_non_empty_subject_is_a_positive_int() {
    // One byte in, one byte counted — strictly inside the curated `int<0, max>`
    // row, which stays the floor for every other subject.
    assert_eq!(dump("non-empty-string", "strlen($v)"), "dumped type: int<1, max> (asserted)");
    assert_eq!(dump("non-falsy-string", "strlen($v)"), "dumped type: int<1, max> (asserted)");
    assert_eq!(dump("numeric-string", "strlen($v)"), "dumped type: int<1, max> (asserted)");
    // A subject with no non-emptiness proof falls through to the envelope.
    assert_eq!(dump("string", "strlen($v)"), "dumped type: int");
    assert_eq!(dump("lowercase-string", "strlen($v)"), "dumped type: int");
}

// Unions are read directly (the #75 survey's nuance, no #74 dependency)

#[test]
fn a_union_of_constant_strings_answers_by_intersecting_its_members() {
    // `'foo'|'bar'` is lowercase on both members, so the trim family carries it.
    assert_eq!(dump("'foo'|'bar'", "trim($v, $v)"), "dumped type: lowercase-string (asserted)");
    // A mixed-casing union carries neither bit, and the rule declines.
    assert_eq!(dump("'foo'|'BAR'", "trim($v, $v)"), "dumped type: string");
    // The length axis works the same way: both members are non-falsy — and since
    // issue #240 the two axes are spelled TOGETHER (`strrev` keeps both), where the
    // old ladder ranked the core rung ahead of the casing half and dropped it.
    assert_eq!(
        dump("'foo'|'bar'", "strrev($v)"),
        "dumped type: non-falsy-lowercase-string (asserted)"
    );
    // …and one falsy member sinks it to non-empty.
    assert_eq!(dump("'foo'|'0'", "strrev($v)"), "dumped type: non-empty-lowercase-string (asserted)");
    // A single constant is the same path with one member (the fold lane owns the
    // all-constant call above this rung; here the charlist is not constant).
    assert_eq!(dump("'ABC'", "trim($v, $v)"), "dumped type: uppercase-string (asserted)");
}

// The admission gate: the engine countersigns, or the rule goes quiet

#[test]
fn a_moved_declaration_withholds_the_rule() {
    // php-src grows a `string|false` arm the rule predates: the pin no longer
    // matches, so the transfer is discarded — a lost refinement, never a wrong one.
    let mut moved = Mock::with_declaration("trim", "string|false");
    assert_eq!(dump_with("lowercase-string", "trim($v)", &mut moved), "dumped type: string");
    // The same engine still admits every OTHER member — the pin is per name.
    assert_eq!(
        dump_with("lowercase-string", "strrev($v)", &mut moved),
        "dumped type: lowercase-string (asserted)"
    );
    // `strlen` pins `int`, and a moved one withholds it too.
    let mut moved_int = Mock::with_declaration("strlen", "int|false");
    assert_eq!(dump_with("non-empty-string", "strlen($v)", &mut moved_int), "dumped type: int");
}

#[test]
fn an_engine_silent_about_the_name_withholds_the_rule() {
    // No declaration, no countersignature (ADR-0061 §2's sidecar-presence leg).
    // The rule withholds; ADR-0069's Asserted declared-return floor answers instead.
    // Its `(asserted)` marker distinguishes it from the transfer's Verified answer.
    // functionMap declares `strtolower` as `lowercase-string`.
    let mut silent = Mock::silent_about("strtolower");
    assert_eq!(
        dump_with("string", "strtolower($v)", &mut silent),
        "dumped type: lowercase-string (asserted)"
    );
    assert_eq!(
        dump_with("string", "strtoupper($v)", &mut silent),
        "dumped type: uppercase-string"
    );
}

#[test]
fn a_project_function_shadowing_the_name_withholds_the_rule() {
    // A9's monkey-patch leg: a simple-named project function called `trim` is not
    // PHP's `trim`, and the rule must not describe it.
    let src = "<?php\nfunction trim(string $s): string { return $s; }\n\
               /** @param lowercase-string $v */\nfunction f(string $v): void { \\PHPStan\\dumpType(trim($v)); }\n";
    let out = one_type_with(src, &mut Mock::sidecar());
    assert_ne!(out, "dumped type: lowercase-string (asserted)");
}

// The `mb_*` exclusion, restated as a test so it cannot drift back in

#[test]
fn no_mb_name_is_a_member() {
    // Encoding- and locale-dependent: the catalog's standing exclusion. nsrt asks
    // for `mb_strtolower`/`mb_substr` right beside their ASCII twins; both stay
    // silent here on purpose.
    let mut m = Mock::sidecar();
    m.types.insert("mb_strtolower".to_owned(), "string".to_owned());
    m.types.insert("mb_substr".to_owned(), "string".to_owned());
    m.facts.insert("mb_strtolower".to_owned(), Fact::General { base: Base::String, nullable: false });
    m.facts.insert("mb_substr".to_owned(), Fact::General { base: Base::String, nullable: false });
    assert_eq!(dump_with("string", "mb_strtolower($v)", &mut m), "dumped type: string");
    assert_eq!(dump_with("lowercase-string", "mb_substr($v, 5)", &mut m), "dumped type: string");
}

// The predicate algebra the table is written in

#[test]
fn the_casing_predicate_is_an_ascii_uppercase_byte_test() {
    // The claim the whole forced leg rests on, asserted against the domain itself:
    // `strtolower('ÄB')` is `'Äb'` at 8.5.8, and that string carries `LOWERCASE`.
    assert!(StrPreds::of("Äb").contains_all(StrPreds::LOWERCASE));
    assert!(!StrPreds::of("ÄB").contains_all(StrPreds::LOWERCASE));
    assert!(StrPreds::of("äB").contains_all(StrPreds::UPPERCASE));
    // …and `''` carries both, which is why the `str_repeat($v, 0)` casing arm and
    // the `implode` empty-array arm are sound.
    assert!(StrPreds::of("").contains_all(StrPreds::LOWERCASE.union(StrPreds::UPPERCASE)));
    assert!(!StrPreds::of("").contains_all(StrPreds::NON_EMPTY));
    // The domain's own witness that a `Val::Str` is what these summaries read.
    // Since ADR-0080 the summary reads **bytes**, so one signature serves a
    // `&str`, a `String` and the `PhpStr` a lowered literal actually carries.
    assert_eq!(StrPreds::of("foo"), StrPreds::of(String::from("foo")));
    assert_eq!(StrPreds::of("foo"), StrPreds::of(PhpStr::from("foo")));
    assert!(matches!(Val::Str("foo".into()), Val::Str(_)));
    // A byte string is uncased, non-numeric and not a decimal-int string — the
    // same answers the engine gives for those bytes.
    let bytes = StrPreds::of(PhpStr::from_bytes(&[0xC0]));
    assert!(bytes.contains_all(StrPreds::LOWERCASE.union(StrPreds::UPPERCASE)));
    assert!(bytes.contains_all(StrPreds::NON_EMPTY.union(StrPreds::NON_FALSY)));
    assert!(!bytes.contains_all(StrPreds::NUMERIC));
}
