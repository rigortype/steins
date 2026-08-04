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

use steins_domain::{Base, Fact, StrPreds, Val};
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
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a string-predicate transfer emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A string-declared fixture: `@param <decl> $v`, one dump of `<expr>`.
fn dump_with(decl: &str, expr: &str, folder: &mut dyn Folder) -> String {
    one_type_with(
        &format!(
            "<?php\n/** @param {decl} $v */\nfunction f(string $v): void {{ \\PHPStan\\dumpType({expr}); }}\n"
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
fn substr_is_casing_only_in_v1() {
    // `substr($lowercase, 0, 5)` is a substring: casing holds.
    assert_eq!(dump("lowercase-string", "substr($v, 5)"), "dumped type: lowercase-string (asserted)");
    assert_eq!(dump("uppercase-string", "substr($v, -5)"), "dumped type: uppercase-string (asserted)");
    assert_eq!(
        dump("lowercase-string", "substr($v, 0, 5)"),
        "dumped type: lowercase-string (asserted)"
    );
    // THE pinned v1 decline: `substr('abc', 0, 0) === ''`, so non-emptiness needs
    // bounds arithmetic this rung does not do — even at the literal `(0, 1)` that
    // upstream PHPStan does answer.
    assert_eq!(dump("non-empty-string", "substr($v, 0, 1)"), "dumped type: string");
    assert_eq!(dump("non-falsy-string", "substr($v, 0, 1)"), "dumped type: string");
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
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v, $flags)")),
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
    //                       '%d' '%b' '%x' '%X' '%o' '%u' '%g' '%G' → '0'
    //                       '%-5s' → '     '  "%'x5s" → 'xxxxx'  '%05d' → '00000'
    //                       '%e' → '0.000000e+0'  '%10.3f' → '     0.000'  '%+d' → '+0'
    //                       "%'-10s" → '----------'  "%-'x10s"/"%1$'x10s" → 'xxxxxxxxxx'
    //   DECLINED (parse)  : '%'  '%s%'  → ValueError "Missing format specifier at end
    //                       of string";  '%z' → ValueError 'Unknown format specifier "z"'
    //
    // Representative cases from that table.
    for claims in ["'abc'", "'100%%'", "'x%sy'", "'%s %s'", "'%2$s %1$s'", "'%s%%'"] {
        assert_eq!(
            dump("string", &format!("sprintf({claims}, $v, $v)")),
            "dumped type: non-empty-string",
            "{claims} emits a literal byte"
        );
    }
    for silent in ["'%s'", "'%.0s'", "'%1$s'", "'%c'", "'%X'", "'%-5s'", "'%10.3f'", "'%+d'"] {
        assert_eq!(
            dump("string", &format!("sprintf({silent}, $v)")),
            "dumped type: string",
            "{silent} can emit nothing"
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
    // The length axis works the same way: both members are non-falsy.
    assert_eq!(dump("'foo'|'bar'", "strrev($v)"), "dumped type: non-falsy-string (asserted)");
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
    assert_eq!(StrPreds::of("foo"), StrPreds::of(&String::from("foo")));
    assert!(matches!(Val::Str("foo".to_owned()), Val::Str(_)));
}
