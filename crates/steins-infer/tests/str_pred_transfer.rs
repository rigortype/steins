//! Issue #77 — the string-predicate transfers at the argument-dependent rung.
//!
//! 25 names declared `string` (`strlen`: `int`): which [`StrPreds`] bits survive
//! the call, or get established by it. Every row below was probed, not
//! reasoned about — each is a decline the rule takes because the real answer
//! contradicts the tidy version.
//!
//! # Behavioral witnesses at `PINNED_PHP` (8.5.8, `php -r`)
//!
//! ```text
//! trim(' A ') === 'A'; ltrim('0abc','0') === 'abc'; trim('  ') === ''
//! substr('abc',0,0) === ''; str_repeat('a',0) === ''; str_repeat('a',2) === 'aa'
//! str_pad('',1) === ' '; str_pad('',0) === ''; str_pad('',-5) === ''
//! strtolower('ÄB') === 'Äb'; strtoupper('äb') === 'äB'   (THE predicate-semantics probe)
//! ucfirst('abc') === 'Abc'; lcfirst('ABC') === 'aBC'; ucwords('ab cd') === 'Ab Cd'
//! strrev('abc') === 'cba'; htmlspecialchars('<') === '&lt;'
//! htmlspecialchars("\x80", ENT_SUBSTITUTE) === "\xEF\xBF\xBD"
//! htmlspecialchars("\x80", ENT_QUOTES) === ''
//! urldecode('%30') === '0'; escapeshellcmd("\x80") === ''
//! sprintf('%.0s','abc') === ''; sprintf('%%') === '%'; sprintf('%X',255) === 'FF'
//! implode(',',[]) === ''
//! ```
//!
//! # Issue #41's additions, witnessed at 8.5.9
//!
//! ```text
//! substr('abc',0) === 'abc'; substr('a',0,2) === 'a'; substr('0x',0,1) === '0'
//! substr('abc',0,-5) === ''; substr('abc',5) === ''
//! strtr('ab','ab','xy') === 'xy'; strtr('a','a','A') === 'A'; strtr('a','ax','0x') === '0'
//! strtr('a',['a'=>'']) === ''; strtr('a',['a'=>'0']) === '0'
//! ```
//!
//! # Issue #41's sprintf NUMERIC slice, witnessed at 8.5.9
//!
//! ```text
//! sprintf('%d',NAN) === '0'; sprintf('%b',1.0e300) === '0'; sprintf('%o','1e400') === '0'
//! sprintf('%+d','not a number') === '+0'
//! sprintf('%f',NAN) === 'NaN'; sprintf('%f',INF) === 'INF'; sprintf('%f','1e400') === 'INF'
//! sprintf('%14x',255) === 'ff'; sprintf('%14X',255) === 'FF'
//! ```
//!
//! `strtolower('ÄB') === 'Äb'` grounds the casing half: Steins' `LOWERCASE` is
//! `php_str_is_lowercase` (no ASCII uppercase byte), which `'Äb'` satisfies, so the
//! predicate holds for *any* input, not only an ASCII one. Zero emission is
//! asserted on every fixture: a transfer is a *type*, never a finding.

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

/// A mock PHP answering the one reflection surface this rung's gate consults (the
/// declaration), plus the envelope the ladder falls back to on a decline.
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
    fn fold(&mut self, _name: &str, _args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
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
    // `untyped.*` is contract-layer claim-absence (issue #200), orthogonal here.
    let other: Vec<&Diagnostic> =
        ds.iter().filter(|d| !d.id.starts_with("debug.") && !d.id.starts_with("untyped.")).collect();
    assert!(other.is_empty(), "a string-predicate transfer emitted a finding: {other:?}");
    let ty: Vec<&Diagnostic> = ds.iter().filter(|d| d.id == DEBUG_TYPE_ID).collect();
    assert_eq!(ty.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ty[0].message.clone()
}

/// A string-declared fixture: `@param <decl> $v`, one dump of `<expr>`. `$n`/`$p`
/// are untyped-but-bound parameters standing in for an *unknown* int or string —
/// no fact, but bound so `variable.undefined` stays silent about them.
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

#[test]
fn the_trim_family_carries_casing_through_any_charlist() {
    // Casing survives removal (output is always a substring), under any charlist too.
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
        assert_eq!(
            dump("lowercase-string", &format!("{f}($v, '0')")),
            "dumped type: lowercase-string (asserted)",
            "{f} keeps casing under an explicit charlist"
        );
        assert_eq!(
            dump("lowercase-string", &format!("{f}($v, $v)")),
            "dumped type: lowercase-string (asserted)"
        );
    }
}

#[test]
fn the_trim_family_never_carries_the_length_axis() {
    // An all-whitespace subject trims to `''`, so the length axis declines, not transfers.
    for f in ["trim", "ltrim", "rtrim", "chop"] {
        assert_eq!(dump("non-empty-string", &format!("{f}($v)")), "dumped type: string");
        assert_eq!(dump("non-falsy-string", &format!("{f}($v)")), "dumped type: string");
    }
}

#[test]
fn substr_carries_casing_from_every_window() {
    // A substring: casing holds wherever the window sits, even where length (below) refuses.
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
    // At offset 0: no length is the identity; length past the end clamps, never
    // pads; length >= 2 keeps non-falsiness, but exactly 1 can still answer `'0'`.
    // Any other offset, an unseen length, or length <= 0 declines.
    assert_eq!(dump("non-empty-string", "substr($v, 0)"), "dumped type: non-empty-string (asserted)");
    assert_eq!(dump("non-falsy-string", "substr($v, 0)"), "dumped type: non-falsy-string (asserted)");
    assert_eq!(
        dump("non-empty-string", "substr($v, 0, 1)"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(
        dump("non-falsy-string", "substr($v, 0, 2)"),
        "dumped type: non-falsy-string (asserted)"
    );
    assert_eq!(
        dump("non-falsy-string", "substr($v, 0, 1)"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(dump("non-empty-string", "substr($v, 0, 0)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "substr($v, 0, -5)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "substr($v, 0, $n)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "substr($v, 1, 1)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "substr($v, $n, 1)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "substr($v, 5)"), "dumped type: string");
}

/// Issue #41 — `strtr` preserves byte COUNT (3-arg) or non-empty replacement
/// (2-arg): non-emptiness survives; nothing else does.
#[test]
fn strtr_carries_non_emptiness_and_refuses_everything_else() {
    // THE refusal, against upstream's own claim: non-falsy degrades to non-empty.
    // Casing never survives (target byte is arbitrary); an unclaimed subject gets nothing.
    assert_eq!(
        dump("non-empty-string", "strtr($v, $p, $p)"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(
        dump("non-falsy-string", "strtr($v, 'ax', '0x')"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(dump("lowercase-string", "strtr($v, $p, $p)"), "dumped type: string");
    assert_eq!(dump("uppercase-string", "strtr($v, $p, $p)"), "dumped type: string");
    assert_eq!(dump("string", "strtr($v, $p, $p)"), "dumped type: string");
}

#[test]
fn the_strtr_array_form_reads_its_replacement_values() {
    // Every value non-empty => non-emptiness survives, but one empty value DELETES,
    // takes the 3-arg form's non-falsy refusal, and an unseen array or wrong arity declines.
    assert_eq!(
        dump("non-empty-string", "strtr($v, ['a' => 'x', 'b' => 'yy'])"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(dump("non-empty-string", "strtr($v, ['a' => 'x', 'b' => ''])"), "dumped type: string");
    assert_eq!(
        dump("non-falsy-string", "strtr($v, ['a' => '0'])"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(dump("non-empty-string", "strtr($v, $p)"), "dumped type: string");
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

#[test]
fn str_repeat_transfers_non_emptiness_only_at_a_multiplier_of_at_least_one() {
    // The SAME subject at multiplier 0 declines (`''` is falsy), as does an unseen multiplier.
    assert_eq!(
        dump("non-empty-string", "str_repeat($v, 2)"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(
        dump("non-falsy-string", "str_repeat($v, 1)"),
        "dumped type: non-falsy-string (asserted)"
    );
    assert_eq!(dump("non-empty-string", "str_repeat($v, 0)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "str_repeat($v, $n)"), "dumped type: string");
}

#[test]
fn str_repeat_carries_casing_at_every_multiplier() {
    // `''` carries both casing bits, so the multiplier-0 gate above doesn't apply here.
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

#[test]
fn str_pad_forces_non_emptiness_at_a_length_of_at_least_one() {
    // True for ANY subject, even one with no fact — but no force below length 1.
    // The subject's own length axis survives regardless, since `str_pad` never shortens.
    assert_eq!(dump("string", "str_pad($v, 5)"), "dumped type: non-empty-string");
    assert_eq!(dump("string", "str_pad($v, 0)"), "dumped type: string");
    assert_eq!(dump("string", "str_pad($v, -5)"), "dumped type: string");
    assert_eq!(dump("string", "str_pad($v, $n)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "str_pad($v, 0)"), "dumped type: non-empty-string (asserted)");
    assert_eq!(dump("non-falsy-string", "str_pad($v, 0)"), "dumped type: non-falsy-string (asserted)");
}

#[test]
fn str_pad_carries_casing_only_when_the_pad_string_carries_it_too() {
    // Default/constant pads are checked directly; an UPPERCASE pad breaks a lowercase
    // subject (inserted verbatim), and an unknown pad drops casing but keeps the length.
    assert_eq!(
        dump("lowercase-string", "str_pad($v, 5)"),
        "dumped type: non-empty-lowercase-string (asserted)"
    );
    assert_eq!(
        dump("lowercase-string", "str_pad($v, 5, '-')"),
        "dumped type: non-empty-lowercase-string (asserted)"
    );
    assert_eq!(dump("lowercase-string", "str_pad($v, 5, 'X')"), "dumped type: non-empty-string (asserted)");
    assert_eq!(dump("lowercase-string", "str_pad($v, 5, $p)"), "dumped type: non-empty-string (asserted)");
}

#[test]
fn strtolower_and_strtoupper_force_their_casing_for_any_subject() {
    // The module-level probe holds for an arbitrary subject, no fact required. The
    // length axis rides along, the OPPOSITE casing is dropped, and an extra arg declines.
    assert_eq!(dump("string", "strtolower($v)"), "dumped type: lowercase-string");
    assert_eq!(dump("string", "strtoupper($v)"), "dumped type: uppercase-string");
    assert_eq!(
        dump("non-empty-string", "strtolower($v)"),
        "dumped type: non-empty-lowercase-string (asserted)"
    );
    assert_eq!(dump("uppercase-string", "strtolower($v)"), "dumped type: lowercase-string (asserted)");
    assert_eq!(dump("lowercase-string", "strtoupper($v)"), "dumped type: uppercase-string (asserted)");
    assert_eq!(dump("string", "strtolower($v, 1)"), "dumped type: string");
}

#[test]
fn ucfirst_breaks_lowercase_and_lcfirst_breaks_uppercase() {
    // THE pinned decline: a lowercase subject does not stay lowercase, but the
    // mirror survives — each function only moves case ONE way.
    assert_eq!(dump("lowercase-string", "ucfirst($v)"), "dumped type: string");
    assert_eq!(dump("lowercase-string", "ucwords($v)"), "dumped type: string");
    assert_eq!(dump("uppercase-string", "lcfirst($v)"), "dumped type: string");
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
        // Casing is NOT claimed: escaping can introduce letters of either case.
        assert_eq!(
            dump("lowercase-string", &format!("{f}($v)")),
            "dumped type: string",
            "{f} makes no casing claim"
        );
    }
    // Unlike the rest, `addcslashes` requires its character list as a 2nd arg.
    assert_eq!(
        dump("non-empty-string", "addcslashes($v, 'a')"),
        "dumped type: non-empty-string (asserted)"
    );
    assert_eq!(dump("non-empty-string", "addcslashes($v)"), "dumped type: string");
}

#[test]
fn urldecode_keeps_non_emptiness_but_not_non_falsiness() {
    // Decoding SHRINKS, so a non-falsy subject can decode to the one falsy string of
    // length 1 — refuting upstream's non-falsy propagation (ADR-0061 §2).
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
    // Upstream PHPStan lists it with the other ten, but it DROPS an invalid
    // multibyte sequence, so a non-empty subject can produce ''. No rule here.
    assert_eq!(dump("non-empty-string", "escapeshellcmd($v)"), "dumped type: string");
    assert_eq!(dump("non-falsy-string", "escapeshellcmd($v)"), "dumped type: string");
}

#[test]
fn htmlspecialchars_needs_the_substitute_flag() {
    // `ENT_SUBSTITUTE` is 8; the 8.1+ default `ENT_QUOTES|ENT_SUBSTITUTE|ENT_HTML401`
    // is 11, so an ABSENT flags arg carries the bit too. A *named* constant declines
    // like any unseen flags arg (no `ArgValue` form); `$n` resolves as bound (issue #41).
    for f in ["htmlspecialchars", "htmlentities"] {
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
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v, ENT_SUBSTITUTE)")),
            "dumped type: string",
            "{f} under a named constant"
        );
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v, 0)")),
            "dumped type: string",
            "{f} without the substitute bit"
        );
        assert_eq!(
            dump("non-empty-string", &format!("{f}($v, $n)")),
            "dumped type: string",
            "{f} under a non-constant flags argument"
        );
    }
}

#[test]
fn implode_carries_casing_from_the_glue_and_every_element() {
    let src = |decl: &str, expr: &str| {
        format!(
            "<?php\n/** @param lowercase-string $g\n * @param {decl} $a */\nfunction f(string $g, array $a): void {{ \\PHPStan\\dumpType({expr}); }}\n"
        )
    };
    // Every contributor (glue and each element) must carry casing, or the claim dies;
    // the one-arg form's glue is `''`, which carries both bits for free.
    assert_eq!(
        one_type_with(&src("array<lowercase-string>", "implode($g, $a)"), &mut Mock::sidecar()),
        "dumped type: lowercase-string (asserted)"
    );
    assert_eq!(
        one_type_with(
            "<?php\n/** @param array<lowercase-string> $a */\nfunction f(string $g, array $a): void { \\PHPStan\\dumpType(implode($g, $a)); }\n",
            &mut Mock::sidecar()
        ),
        "dumped type: string"
    );
    assert_eq!(
        one_type_with(&src("array<string>", "implode($g, $a)"), &mut Mock::sidecar()),
        "dumped type: string"
    );
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
    // A non-empty element proves nothing while the array itself may be empty.
    assert_eq!(
        one_type_with(
            "<?php\n/** @param non-empty-array<non-empty-string> $a */\nfunction f(array $a): void { \\PHPStan\\dumpType(implode(',', $a)); }\n",
            &mut Mock::sidecar()
        ),
        "dumped type: string"
    );
}

#[test]
fn sprintf_claims_non_emptiness_only_from_a_literal_byte() {
    // A literal byte anywhere in a constant format (even just `%%`) earns NON_EMPTY;
    // an all-conversion or non-constant format claims nothing, and none claims casing.
    assert_eq!(
        dump("string", "sprintf('%s0%s', $v, $v)"),
        "dumped type: non-empty-string"
    );
    assert_eq!(dump("string", "sprintf('Hello %s', $v)"), "dumped type: non-empty-string");
    assert_eq!(dump("string", "sprintf('%%%s', $v)"), "dumped type: non-empty-string");
    assert_eq!(dump("string", "sprintf('%s', $v)"), "dumped type: string");
    assert_eq!(dump("string", "sprintf('%.0s', $v)"), "dumped type: string");
    assert_eq!(dump("string", "sprintf(\"%'x5s\", $v)"), "dumped type: string");
    assert_eq!(dump("non-empty-string", "sprintf($v, $v)"), "dumped type: string");
    assert_eq!(dump("lowercase-string", "sprintf('a%s', $v)"), "dumped type: non-empty-string (asserted)");
}

#[test]
fn the_sprintf_format_scanner_matches_the_engine_on_every_probed_shape() {
    // Differentialled against 8.5.8 over the whole documented format grammar, each
    // called with EMPTY-STRING args (worst case for NON_EMPTY); representative
    // cases below, all matching the engine. Issue #41 moved `%d`/`%b`/`%o`/`%05d`/
    // `%+d` from "claims nothing" to NUMERIC (the int cast never renders a
    // non-digit byte, even from a plain string); `%e`/`%f`/`%g` need a proven
    // `int` first — see `sprintf_gates_the_float_trio_on_a_proven_int_value`.
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
    for numeric in ["'%d'", "'%b'", "'%o'", "'%05d'", "'%+d'"] {
        assert_eq!(
            dump("string", &format!("sprintf({numeric}, $v)")),
            "dumped type: numeric-string",
            "{numeric} is an unconditional int-cast conversion"
        );
    }
    // A format the engine itself refuses (ValueError) is refused here too.
    for refused in ["'%'", "'%s%'", "'%z'"] {
        assert_eq!(
            dump("string", &format!("sprintf({refused}, $v)")),
            "dumped type: string",
            "{refused} is a ValueError at 8.5.8"
        );
    }
}

/// Mirrors `dump`, but with a NATIVE `int` parameter — issue #41's float-trio gate
/// needs a genuinely `int`-typed value, which a docblock narrowing can't fake.
fn dump_int(expr: &str) -> String {
    one_type_with(
        &format!("<?php\nfunction f(int $v): void {{ \\PHPStan\\dumpType({expr}); }}\n"),
        &mut Mock::sidecar(),
    )
}

/// Issue #41 — the int-cast trio forces `NUMERIC` unconditionally, as
/// [`the_sprintf_format_scanner_matches_the_engine_on_every_probed_shape`] already
/// showed from a `string` value; pins the same claim from a proven `int` one.
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

/// Issue #41 — the float-format trio forces `NUMERIC` only when the value is
/// provably `int`. A `string` value declines even though the scanner can't see
/// through it: PHP's float formatter renders `NAN`/`INF` verbatim, and a numeric
/// STRING can overflow its own `(float)` cast to `INF` — a native `int` can hold
/// neither special value, closing both holes at once.
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

/// Issue #41 — `%x`/`%X` are int-cast conversions like `%d`/`%b`/`%o`, but their
/// digits are hexadecimal — never a PHP numeric string — so both are refused
/// regardless of value type, against upstream PHPStan's `bug-7387.php` fixture
/// which wrongly claims `numeric-string` for both.
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
    // Literal text still earns `NON_EMPTY` (the existing rule), never `NUMERIC` —
    // the claim is about the WHOLE format. `'%%d'`: `%%` and `d` are both literal bytes.
    for f in ["'literal %d'", "'%d literal'", "' %d'", "'%d %d'", "'%%d'"] {
        assert_eq!(
            dump_int(&format!("sprintf({f}, $v, $v)")),
            "dumped type: non-empty-string",
            "{f} carries a literal byte, so NON_EMPTY holds but not NUMERIC"
        );
    }
    // Not ONE admitted conversion either: no literal byte survives, both legs decline.
    for f in ["\"%'*10d\"", "'%1$d'", "'%d%d'"] {
        assert_eq!(
            dump_int(&format!("sprintf({f}, $v, $v)")),
            "dumped type: string",
            "{f} is neither a literal byte nor a single admitted conversion"
        );
    }
}

/// Issue #41 — `vsprintf` reads the same scanner as `sprintf` but only ever
/// admits the int-cast trio (matching PHPStan's `bug-7387.php` fixture): it need
/// not open the values array, unlike the float trio and hex pair, which stay declined.
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
/// the same call, the rule wins, and only by narrowing strictly inside the row.
/// `strlen` is the family's one such collision; the fixture hands the walk what
/// ADR-0056's gate admits, `("strlen", "int<0, max>")` — the curated row itself.
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
    // The rule REPLACES the row when strictly narrower (`int<1, max>` inside
    // `int<0, max>`); where it declines, the curated row is untouched.
    assert_eq!(
        dump_with("non-empty-string", "strlen($v)", &mut curated()),
        "dumped type: int<1, max> (asserted)"
    );
    assert_eq!(dump_with("string", "strlen($v)", &mut curated()), "dumped type: int<0, max>");
}

#[test]
fn strlen_of_a_non_empty_subject_is_a_positive_int() {
    // One byte in, one counted — strictly inside curated `int<0, max>`; no
    // non-emptiness proof falls through to that envelope.
    assert_eq!(dump("non-empty-string", "strlen($v)"), "dumped type: int<1, max> (asserted)");
    assert_eq!(dump("non-falsy-string", "strlen($v)"), "dumped type: int<1, max> (asserted)");
    assert_eq!(dump("numeric-string", "strlen($v)"), "dumped type: int<1, max> (asserted)");
    assert_eq!(dump("string", "strlen($v)"), "dumped type: int");
    assert_eq!(dump("lowercase-string", "strlen($v)"), "dumped type: int");
}

#[test]
fn a_union_of_constant_strings_answers_by_intersecting_its_members() {
    // A union intersects its members' bits (mixed casing carries neither); since
    // issue #240 the two axes are spelled TOGETHER (old ladder dropped one), and a
    // single constant is the same path with one member, non-constant charlist.
    assert_eq!(dump("'foo'|'bar'", "trim($v, $v)"), "dumped type: lowercase-string (asserted)");
    assert_eq!(dump("'foo'|'BAR'", "trim($v, $v)"), "dumped type: string");
    assert_eq!(
        dump("'foo'|'bar'", "strrev($v)"),
        "dumped type: non-falsy-lowercase-string (asserted)"
    );
    assert_eq!(dump("'foo'|'0'", "strrev($v)"), "dumped type: non-empty-lowercase-string (asserted)");
    assert_eq!(dump("'ABC'", "trim($v, $v)"), "dumped type: uppercase-string (asserted)");
}

#[test]
fn a_moved_declaration_withholds_the_rule() {
    // php-src grows a `string|false` arm the rule predates: the pin no longer
    // matches (a lost refinement, never a wrong one), but every OTHER member still admits.
    let mut moved = Mock::with_declaration("trim", "string|false");
    assert_eq!(dump_with("lowercase-string", "trim($v)", &mut moved), "dumped type: string");
    assert_eq!(
        dump_with("lowercase-string", "strrev($v)", &mut moved),
        "dumped type: lowercase-string (asserted)"
    );
    let mut moved_int = Mock::with_declaration("strlen", "int|false");
    assert_eq!(dump_with("non-empty-string", "strlen($v)", &mut moved_int), "dumped type: int");
}

#[test]
fn an_engine_silent_about_the_name_withholds_the_rule() {
    // No declaration, no countersignature (ADR-0061 §2): the rule withholds and
    // ADR-0069's Asserted floor answers instead (`(asserted)` marks it apart from
    // Verified); functionMap itself declares `strtolower` as `lowercase-string`.
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
    // A9's monkey-patch leg: a project function named `trim` is not PHP's `trim`.
    let src = "<?php\nfunction trim(string $s): string { return $s; }\n\
               /** @param lowercase-string $v */\nfunction f(string $v): void { \\PHPStan\\dumpType(trim($v)); }\n";
    let out = one_type_with(src, &mut Mock::sidecar());
    assert_ne!(out, "dumped type: lowercase-string (asserted)");
}

#[test]
fn no_mb_name_is_a_member() {
    // Encoding/locale-dependent: the catalog's standing exclusion (nsrt asks for
    // these right beside their ASCII twins).
    let mut m = Mock::sidecar();
    m.types.insert("mb_strtolower".to_owned(), "string".to_owned());
    m.types.insert("mb_substr".to_owned(), "string".to_owned());
    m.facts.insert("mb_strtolower".to_owned(), Fact::General { base: Base::String, nullable: false });
    m.facts.insert("mb_substr".to_owned(), Fact::General { base: Base::String, nullable: false });
    assert_eq!(dump_with("string", "mb_strtolower($v)", &mut m), "dumped type: string");
    assert_eq!(dump_with("lowercase-string", "mb_substr($v, 5)", &mut m), "dumped type: string");
}

#[test]
fn the_casing_predicate_is_an_ascii_uppercase_byte_test() {
    // The claim the whole forced leg rests on, asserted against the domain itself.
    assert!(StrPreds::of("Äb").contains_all(StrPreds::LOWERCASE));
    assert!(!StrPreds::of("ÄB").contains_all(StrPreds::LOWERCASE));
    assert!(StrPreds::of("äB").contains_all(StrPreds::UPPERCASE));
    // `''` carries both, grounding the `str_repeat($v, 0)` and `implode` empty-array arms.
    assert!(StrPreds::of("").contains_all(StrPreds::LOWERCASE.union(StrPreds::UPPERCASE)));
    assert!(!StrPreds::of("").contains_all(StrPreds::NON_EMPTY));
    // Since ADR-0080 the summary reads **bytes**: one signature serves `&str`/`String`.
    assert_eq!(StrPreds::of("foo"), StrPreds::of(String::from("foo")));
    assert_eq!(StrPreds::of("foo"), StrPreds::of(PhpStr::from("foo")));
    assert!(matches!(Val::Str("foo".into()), Val::Str(_)));
    // A byte string is uncased, non-numeric, not a decimal-int string.
    let bytes = StrPreds::of(PhpStr::from_bytes(&[0xC0]));
    assert!(bytes.contains_all(StrPreds::LOWERCASE.union(StrPreds::UPPERCASE)));
    assert!(bytes.contains_all(StrPreds::NON_EMPTY.union(StrPreds::NON_FALSY)));
    assert!(!bytes.contains_all(StrPreds::NUMERIC));
}
