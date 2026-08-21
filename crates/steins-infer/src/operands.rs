//! `type.invalid-operand` (ADR-0078 / issue #191): binary and unary operators whose
//! operand the lanes prove PHP 8 rejects with a `TypeError` — arrays, objects and
//! non-numeric strings against arithmetic, `.`, bitwise and unary operators.

use std::collections::HashMap;

use steins_domain::{Base, Fact, Val};
use steins_syntax::{
    ArgValue, BinaryOperandOp, OperandSite, OperandSiteKind, Stmt, StmtKind, UnaryOperandOp,
};

use crate::cx::Cx;
use crate::env::{Known, Stratum};
use crate::project::Diagnostic;
use crate::INVALID_OPERAND_ID;

// ---------------------------------------------------------------------------
// `type.invalid-operand` (ADR-0078, issue #191).
// ---------------------------------------------------------------------------

/// A **proven** operand kind — the row key of [`INVALID_OPERAND_ID`]'s witnessed
/// table. An operand the value domain cannot pin to exactly one of these has no
/// kind at all (`None`), and a site with any kindless operand is silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandKind {
    /// A proven array (a literal, a `Fact::Shape`, a `Val::Array`).
    Array,
    /// A proven string with **no leading numeric prefix** — the one string shape
    /// PHP refuses fatally in arithmetic (see [`string_is_fatal_operand`]).
    FatalString,
    /// A proven string that is not `FatalString`: a numeric string, a
    /// leading-numeric string (warning-grade, not this id), or an abstract
    /// `string` base whose content the domain does not know. Never a fatal
    /// premise — but it IS the "the other operand is a string" premise the
    /// byte-wise `& | ^` legality rests on.
    String,
    /// A proven `int`.
    Int,
    /// A proven `float`.
    Float,
    /// A proven `bool`; the truth value when the domain pins it, because the
    /// `~` sentence names `true`/`false` and never the word `bool`.
    Bool(Option<bool>),
    /// A proven `null`.
    Null,
}

impl OperandKind {
    /// The word PHP's `Unsupported operand types: {l} {op} {r}` sentence uses for
    /// this kind (witnessed at 8.5.9: a bool operand renders `bool` there, unlike
    /// the bitwise-not sentence).
    fn word(self) -> &'static str {
        match self {
            OperandKind::Array => "array",
            OperandKind::FatalString | OperandKind::String => "string",
            OperandKind::Int => "int",
            OperandKind::Float => "float",
            OperandKind::Bool(_) => "bool",
            OperandKind::Null => "null",
        }
    }

    /// The word PHP's `Cannot perform bitwise not on {word}` sentence uses, or
    /// `None` when the domain cannot pin it — a `bool` base with no truth value
    /// is proven fatal but has no quotable word, so the finding drops the quote
    /// rather than fabricating one (the `foreach.non-iterable` precedent).
    fn bitnot_word(self) -> Option<&'static str> {
        match self {
            OperandKind::Array => Some("array"),
            OperandKind::Null => Some("null"),
            OperandKind::Bool(Some(true)) => Some("true"),
            OperandKind::Bool(Some(false)) => Some("false"),
            _ => None,
        }
    }
}

/// Whether a **known** string is one PHP refuses fatally as an arithmetic
/// operand: a string with no *leading numeric prefix* at all.
///
/// PHP 8 grades string operands in three bands (all `php -r`-witnessed at 8.5.9
/// against `$s + 1`), and only the third is this id's:
/// * **numeric** (`'5'`, `'5.5'`, `'.5'`, `'5.'`, `'1e3'`, `'017'`, `'+5'`,
///   `' 5'`, `'5 '`, `'000123'`) — legal, silently coerced;
/// * **leading-numeric** (`'5abc'`, `'.5abc'`, `'1e'`, `'0x1A'`, `'0b11'`,
///   `'1_000'`, `"5\0"`) — an `E_WARNING` and the operation still computes.
///   Warning-grade, not this id, at any posture;
/// * **no numeric prefix** (`'abc'`, `''`, `' '`, `'abc5'`, `'.abc'`, `'e5'`,
///   `'-abc'`, `'--5'`, `'INF'`, `'NAN'`, `"\0 5"`, `'０'`) — `TypeError:
///   Unsupported operand types: string + int`. This function's `true`.
///
/// The prefix grammar is PHP's own, matched exactly: optional leading
/// whitespace from `" \t\n\r\x0b\x0c"` (a NBSP is *not* whitespace —
/// `"\u{a0}5" + 1` is a `TypeError`), then an optional single `+`/`-`, then an
/// ASCII digit or a `.` followed by one. Anything else has no prefix and is
/// fatal. Witnessed on both sides: `'- 5'` (sign, space, digit) is fatal,
/// `'-.5x'` is the warning.
fn string_is_fatal_operand(s: impl AsRef<[u8]>) -> bool {
    // Byte-oriented, and byte-**exact** for a non-UTF-8 string: PHP's own
    // leading-numeric prefix rule reads bytes, so `"\xC0" * 2` is the TypeError
    // this reports and `"5\xC0" * 2` is the mere warning it does not
    // (verified on PHP 8.5). No decline is needed here (ADR-0080 §2.5).
    let b = s.as_ref();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    if i < b.len() && matches!(b[i], b'+' | b'-') {
        i += 1;
    }
    let leading_numeric = match b.get(i) {
        Some(c) if c.is_ascii_digit() => true,
        Some(b'.') => b.get(i + 1).is_some_and(u8::is_ascii_digit),
        _ => false,
    };
    !leading_numeric
}

/// The operand kind of a concrete value.
fn val_operand_kind(v: &Val) -> OperandKind {
    match v {
        Val::Int(_) => OperandKind::Int,
        Val::Float(_) => OperandKind::Float,
        Val::Bool(b) => OperandKind::Bool(Some(*b)),
        Val::Null => OperandKind::Null,
        Val::Array(_) => OperandKind::Array,
        Val::Str(s) => {
            if string_is_fatal_operand(s) {
                OperandKind::FatalString
            } else {
                OperandKind::String
            }
        }
    }
}

/// The operand kind a whole [`Fact`] proves, or `None` when it proves no single
/// one. Two families decline, both because their denotation spans two
/// *different* consequences: any `nullable: true` layer (`?string` is
/// `string`-or-`null`, opposite verdicts for `~`), and a heterogeneous `OneOf`
/// (`1|'abc'` from a ternary — a homogeneous one still answers, so `'abc'|'def'`
/// is a proven `FatalString`).
///
/// An abstract `string` base answers [`OperandKind::String`], never
/// `FatalString`: `numeric-string` and `'abc'` share that layer, and only the
/// content decides. Conservative direction — suppresses a fatal claim, and
/// supplies the "is a string" premise the `& | ^` rows need.
fn fact_operand_kind(fact: &Fact) -> Option<OperandKind> {
    match fact {
        Fact::Singleton(v) => Some(val_operand_kind(v)),
        Fact::OneOf(vals) => {
            let first = val_operand_kind(vals.first()?);
            vals.iter().all(|v| val_operand_kind(v) == first).then_some(first)
        }
        // Several operand kinds at once — no single row of the operator table
        // applies, so the fatal is not proven.
        Fact::Union { .. } => None,
        Fact::Shape { nullable: false, .. } => Some(OperandKind::Array),
        Fact::Refined { base, nullable: false, .. } | Fact::General { base, nullable: false } => {
            Some(match base {
                Base::Int => OperandKind::Int,
                Base::Float => OperandKind::Float,
                Base::String => OperandKind::String,
                // No truth value at this layer, so no `~` word — the kind still
                // proves the fatal, `bitnot_word` just declines to quote it.
                Base::Bool => OperandKind::Bool(None),
            })
        }
        Fact::Refined { nullable: true, .. }
        | Fact::General { nullable: true, .. }
        | Fact::Shape { nullable: true, .. } => None,
    }
}

/// The operand kind of one lowered operand, resolved against the enclosing
/// statement's entry env.
///
/// Exactly two lanes carry a proof: a literal in the source (`[] + 1`), and a
/// bare `$var` whose env fact is `Verified` — the `call.on-non-object` premise
/// style. An `Asserted` fact (a `@phpstan-assert string $x`) cannot premise a
/// proof-layer fatal (ADR-0052 §5), and everything else — a call result, a
/// property fetch, an offset read, a nested operator application, an object —
/// is `None`, which is silence.
fn operand_kind(v: &ArgValue, env: &HashMap<String, Known>) -> Option<OperandKind> {
    match v {
        ArgValue::Int(_) => Some(OperandKind::Int),
        ArgValue::Float(_) => Some(OperandKind::Float),
        ArgValue::Bool(b) => Some(OperandKind::Bool(Some(*b))),
        ArgValue::Null => Some(OperandKind::Null),
        ArgValue::Str(s) => Some(if string_is_fatal_operand(s) {
            OperandKind::FatalString
        } else {
            OperandKind::String
        }),
        ArgValue::Array(_) => Some(OperandKind::Array),
        ArgValue::Var(name) => {
            let k = env.get(name)?;
            if k.stratum != Stratum::Verified {
                return None;
            }
            fact_operand_kind(k.fact.as_ref()?)
        }
        _ => None,
    }
}

/// The binary half of the witnessed table (see [`INVALID_OPERAND_ID`] for every
/// row and its `php -r` witness). `true` means: this operand pair is a
/// guaranteed `TypeError` on PHP 8.1…8.5.
fn binary_operands_are_fatal(op: BinaryOperandOp, l: OperandKind, r: OperandKind) -> bool {
    let array = |k| matches!(k, OperandKind::Array);
    let fatal_str = |k| matches!(k, OperandKind::FatalString);
    let string = |k| matches!(k, OperandKind::FatalString | OperandKind::String);
    match op {
        // `+` is the one operator with an array survivor: `[] + []` is the array
        // UNION, not arithmetic. Every other array pairing fatals.
        BinaryOperandOp::Add => {
            if array(l) || array(r) {
                return !(array(l) && array(r));
            }
            fatal_str(l) || fatal_str(r)
        }
        // Arithmetic and shifts: an array or a prefix-less string on EITHER side
        // is fatal against every other kind, `array - array` and
        // `'abc' << '5'` included.
        BinaryOperandOp::Sub
        | BinaryOperandOp::Mul
        | BinaryOperandOp::Div
        | BinaryOperandOp::Mod
        | BinaryOperandOp::Pow
        | BinaryOperandOp::ShiftLeft
        | BinaryOperandOp::ShiftRight => {
            array(l) || array(r) || fatal_str(l) || fatal_str(r)
        }
        // `& | ^` are TWO operators sharing a spelling: the integer one, and the
        // byte-wise string one that runs when BOTH operands are strings. So a
        // prefix-less string is fatal only against a proven non-string —
        // `'abc' & 'abc'` is `'abc'`, witnessed.
        BinaryOperandOp::BitAnd | BinaryOperandOp::BitOr | BinaryOperandOp::BitXor => {
            if array(l) || array(r) {
                return true;
            }
            (fatal_str(l) && !string(r)) || (fatal_str(r) && !string(l))
        }
    }
}

/// The unary half of the witnessed table. `~` is the odd one: it refuses
/// `bool`/`null` (which every arithmetic operator happily coerces) and accepts
/// any string (the byte-wise complement, `~'abc'`).
fn unary_operand_is_fatal(op: UnaryOperandOp, k: OperandKind) -> bool {
    match op {
        UnaryOperandOp::Minus | UnaryOperandOp::Plus => {
            matches!(k, OperandKind::Array | OperandKind::FatalString)
        }
        UnaryOperandOp::BitNot => {
            matches!(k, OperandKind::Array | OperandKind::Bool(_) | OperandKind::Null)
        }
    }
}

/// Judge one [`OperandSite`] against the entry env and emit
/// [`INVALID_OPERAND_ID`] when the proven operand kinds land on a witnessed
/// fatal row (ADR-0078, issue #191).
///
/// **Every** operand must be proven. That is not timidity about unknowns, it is
/// the table's own shape: a row is keyed on the *pair*, and the pair is what was
/// witnessed. It also buys the object posture for free — an operand that might
/// be a `GMP` (whose internal handlers DO overload arithmetic) never has a kind.
fn judge_operand_site(
    cx: &Cx,
    site: &OperandSite,
    env: &HashMap<String, Known>,
    out: &mut Vec<Diagnostic>,
) {
    let message = match &site.kind {
        OperandSiteKind::Binary { op, lhs, rhs } => {
            let (Some(l), Some(r)) = (operand_kind(lhs, env), operand_kind(rhs, env)) else {
                return;
            };
            if !binary_operands_are_fatal(*op, l, r) {
                return;
            }
            let (sym, lw, rw) = (op.symbol(), l.word(), r.word());
            format!(
                "{} {sym} {} — proven {lw} {sym} {rw} on this path — proven TypeError \
                 (Unsupported operand types: {lw} {sym} {rw})",
                lhs.render(),
                rhs.render(),
            )
        }
        OperandSiteKind::Unary { op, operand } => {
            let Some(k) = operand_kind(operand, env) else { return };
            if !unary_operand_is_fatal(*op, k) {
                return;
            }
            let (sym, word, shown) = (op.symbol(), k.word(), operand.render());
            match op {
                // The engine compiles unary `-`/`+` as a multiplication, so its
                // own sentence names `*` and an `int` right operand that is
                // nowhere in the source: `-[]` reports "Unsupported operand
                // types: array * int" (witnessed). Quoted verbatim rather than
                // paraphrased — the sentence is what a reader will grep for.
                UnaryOperandOp::Minus | UnaryOperandOp::Plus => format!(
                    "{sym}{shown} — {shown} is proven {word} on this path — proven TypeError \
                     (Unsupported operand types: {word} * int)"
                ),
                UnaryOperandOp::BitNot => match k.bitnot_word() {
                    Some(w) => format!(
                        "~{shown} — {shown} is proven {word} on this path — proven TypeError \
                         (Cannot perform bitwise not on {w})"
                    ),
                    None => format!(
                        "~{shown} — {shown} is proven {word} on this path — PHP refuses bitwise \
                         not on a {word} (proven TypeError)"
                    ),
                },
            }
        }
    };
    let pos = cx.tree().position(site.span.start);
    out.push(Diagnostic {
        id: INVALID_OPERAND_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message,
        facet: None,
        fix: None,
    });
}

/// Judge every [`OperandSite`] the statement covers, against the statement's
/// ENTRY env (ADR-0078, issue #191).
///
/// The reach is the narrowest one that is *always* reading the right env:
/// **leaf statements only** — a structured `If`/`Match` and an `Opaque`
/// construct both textually contain their body's sites, and the entry env is
/// not the env those bodies run under (`$x = 1; while (…) { $x = []; $x + 1;
/// }`). The `If`/`Match` branches are walked as their own inner statements, so
/// their leaf statements are judged there with the branch env and nothing is
/// judged twice; a loop/`try` body is simply out of reach, like every other
/// ADR-0027 unmodelled construct (`Barrier` excluded for the same reason). And
/// **same function body**: a closure's sites lie inside their creating
/// statement's span, but `$s` inside `fn () => $s + 1` is the closure's own
/// binding — [`OperandSite::enclosing_body`] tells the two apart, and the
/// closure's own scope walk judges those sites with its own env.
///
/// The sites are span-sorted, so the covered range is found by binary search
/// rather than a per-statement scan of the file.
pub(crate) fn check_operand_sites(
    cx: &Cx,
    stmt: &Stmt,
    env: &HashMap<String, Known>,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    if poisoned {
        return;
    }
    if !matches!(
        stmt.kind,
        StmtKind::Assign { .. }
            | StmtKind::PropAssign { .. }
            | StmtKind::Call(_)
            | StmtKind::Return { .. }
            | StmtKind::Echo(_)
            | StmtKind::Assert { .. }
            | StmtKind::Throw { .. }
            | StmtKind::OffsetWrite { .. }
            | StmtKind::OffsetUnset { .. }
    ) {
        return;
    }
    let sites = cx.tree().operand_sites();
    let from = sites.partition_point(|s| s.span.start < stmt.span.start);
    for site in &sites[from..] {
        if site.span.start > stmt.span.end {
            break;
        }
        if site.span.end > stmt.span.end {
            continue;
        }
        if let Some(body) = site.enclosing_body
            && (body.start > stmt.span.start || stmt.span.end > body.end)
        {
            continue;
        }
        judge_operand_site(cx, site, env, out);
    }
}

// end invalid operands (ADR-0078, issue #191)
