//! The **user code a builtin can reach through its arguments** (issue #856,
//! ADR-0021's second 2026-10-01 amendment): [`arg_reach`] and its per-position
//! answer, [`ArgReach`].
//!
//! A builtin whose own effects are certified can still hand control to
//! userland through what it was given. A coercive `string` parameter converts
//! an object through `__toString`; `count()` calls `Countable::count`;
//! `in_array()` compares loosely and `implode()` renders each piece; a
//! callable runs; `is_callable('Foo::bar')` autoloads. Each of those is the
//! callee running code the effects pass never saw, so a call is pure only
//! where the call site rules every reaching argument out.
//!
//! The row is **derived from the mined arginfo** ([`param_facts`]), and a short
//! curated list overrides the derivation where php-src shows a builtin does
//! less with a parameter than its type admits. The derivation, per position:
//!
//! | Declared member | Reach | Why |
//! |---|---|---|
//! | `int`, `float`, `bool`, `false`, `null`, … | [`ArgReach::Inert`] | a `TypeError` |
//! | `string` | [`ArgReach::Coerced`] | `__toString`, under coercive typing |
//! | a class, an interface, `object`, `iterable` | [`ArgReach::Object`] | used as itself |
//! | `array`, `mixed` | [`ArgReach::Nested`] | the builtin's own business |
//! | a declared `callable` position | [`ArgReach::Callback`] | it runs |
//!
//! A union takes its strongest member's reach, a variadic tail repeats its
//! last position, and a position past a non-variadic list is an
//! `ArgumentCountError` raised before anything runs. A position the
//! resource table says demands a resource is inert: an object there is a
//! `TypeError`. The certified pure families answer inert everywhere, which is
//! what certified means.
//!
//! Witnessed on PHP 8.5.11 for every override and every reach kind, in both
//! calling modes: `strlen($o)` runs `__toString` and is a `TypeError` under
//! `strict_types=1`; `strval($o)`, `sprintf('%s', $o)`, `implode(',', [$o])`,
//! `in_array('x', [$o])` and `json_encode([$j])` run user code in both modes;
//! `intval($o)`, `boolval($o)`, `gettype($o)`, `array_fill(0, 1, $o)`,
//! `array_merge([$o])` and `count([$c], COUNT_RECURSIVE)` run none.
//!
//! Engine **methods and constructors** are the same question asked of a
//! `Class::method` key: [`method_arg_reach`], in `reach/methods.rs`.
//!
//! [`param_facts`]: crate::param_facts

mod methods;
pub use methods::{MethodReachRow, method_arg_reach};

use crate::builtins::{ParamFacts, resource_param};
use crate::effects::certified_pure;

/// What user code a builtin can reach through one argument position, from
/// none to the most, and what rules it out at a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArgReach {
    /// None, for every value the parameter admits.
    Inert,
    /// A coercive `string` parameter: an object argument is converted through
    /// its `__toString`. Ruled out by an argument shown not to be an object,
    /// or by `declare(strict_types=1)` in the calling file, where the object
    /// is a `TypeError` instead.
    Coerced,
    /// The builtin converts, counts or reads an object argument itself, in
    /// either calling mode (`strval`'s `__toString`, `count`'s
    /// `Countable::count`, a lazy object's initializer). Ruled out by an
    /// argument shown not to be an object.
    Object,
    /// The builtin also converts or compares what an array argument holds
    /// (`implode`'s pieces, `in_array`'s haystack, `json_encode`'s values).
    /// Ruled out only by an argument shown to hold no object at any depth.
    Nested,
    /// A callable: the call runs it. Never ruled out at a call site the
    /// effects pass sees as a plain call; a callback it can resolve takes
    /// another road.
    Callback,
    /// A class or method named in a string is looked up, which runs the
    /// autoloader. Never ruled out.
    Autoload,
}

/// One builtin's [`ArgReach`] row ([`arg_reach`]).
#[derive(Debug, Clone, Copy)]
pub struct ReachRow {
    facts: &'static ParamFacts,
    name: &'static str,
    certified: bool,
}

impl ReachRow {
    /// The reach of the argument at `position`.
    #[must_use]
    pub fn at(&self, position: usize) -> ArgReach {
        if self.certified {
            return ArgReach::Inert;
        }
        let last = self.facts.params.len().checked_sub(1);
        let index = match last {
            Some(last) if position > last && self.facts.variadic.contains(&last) => last,
            _ if position < self.facts.params.len() => position,
            _ => return ArgReach::Inert,
        };
        if let Some(&(_, reach)) = overrides(self.name).iter().find(|(p, _)| *p == index) {
            return reach;
        }
        if self.facts.callable.contains(&index) {
            return ArgReach::Callback;
        }
        if resource_param(self.name, index).is_some() {
            return ArgReach::Inert;
        }
        declared_reach(self.facts.params[index])
    }

    /// Whether some position reaches user code that the calling file's
    /// strictness alone does not rule out: the question for a call whose
    /// positions cannot be read, a named or spread argument list or a
    /// builtin handed over as a callback.
    #[must_use]
    pub fn reaches_blind(&self, strict: bool) -> bool {
        (0..self.facts.params.len()).any(|p| reaches_past_strictness(self.at(p), strict))
    }
}

/// Whether `reach` runs user code that the calling file's strictness alone
/// does not rule out ([`ReachRow::reaches_blind`] over one position).
fn reaches_past_strictness(reach: ArgReach, strict: bool) -> bool {
    match reach {
        ArgReach::Inert => false,
        ArgReach::Coerced => !strict,
        _ => true,
    }
}

/// The reach row of the builtin `name` (case-insensitive), or `None` when the
/// mining build had no such function.
///
/// The row answers what the **arguments** can reach and nothing else: a
/// builtin that runs user code by its own design (`unserialize`'s magic
/// methods, `spl_autoload_call`) is not described by it. The effects pass asks
/// it only of a name whose own effects the catalog already states.
#[must_use]
pub fn arg_reach(name: &str) -> Option<ReachRow> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    let rows = crate::param_facts_generated::PARAM_FACTS;
    let i = rows.binary_search_by(|(n, _)| (*n).cmp(key.as_str())).ok()?;
    let (name, facts) = &rows[i];
    Some(ReachRow { facts, name, certified: certified_pure(name) })
}

/// The reach a declared type admits ([`ArgReach`]'s derivation table).
fn declared_reach(ty: &str) -> ArgReach {
    ty.split('|')
        .map(|member| member.trim().trim_start_matches('?'))
        .map(|member| match member.to_ascii_lowercase().as_str() {
            "int" | "float" | "bool" | "true" | "false" | "null" => ArgReach::Inert,
            "string" => ArgReach::Coerced,
            "array" | "mixed" => ArgReach::Nested,
            _ => ArgReach::Object,
        })
        .max()
        .unwrap_or(ArgReach::Nested)
}

/// Where a printf-family builtin keeps its format string and how the values
/// the format names reach it ([`printf_family`]).
///
/// The family's value positions are not a declared-type question: `sprintf`
/// converts an object through `__toString` only where the format says `%s`,
/// and every numeric conversion turns it into a number with a warning. The
/// row's `Object` at the first value is the reading for a format the call
/// site cannot read; a literal format is read by [`format_reach`], and this
/// type maps its answer back onto call positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintfFamily {
    format: usize,
    vector: bool,
}

impl PrintfFamily {
    /// The 0-based call position of the format string.
    #[must_use]
    pub const fn format_position(&self) -> usize {
        self.format
    }

    /// Whether the values arrive as one array (`vsprintf`, `vprintf`) and not
    /// as the positions after the format.
    #[must_use]
    pub const fn is_vector(&self) -> bool {
        self.vector
    }

    /// The reach of the argument at call `position`, given a literal format's
    /// per-value reaches ([`format_reach`]), or `None` for a position at or
    /// before the format, which the builtin's own row answers.
    ///
    /// A value no conversion names is never read, so it is
    /// [`ArgReach::Inert`] whatever it holds. A vector is
    /// [`ArgReach::Nested`] when some conversion is `%s` (an element of the
    /// array is rendered) and [`ArgReach::Inert`] when none is. A position past
    /// the vector is an `ArgumentCountError` raised before anything runs.
    #[must_use]
    pub fn reach_at(&self, per_value: &[ArgReach], position: usize) -> Option<ArgReach> {
        let value = position.checked_sub(self.format + 1)?;
        if !self.vector {
            return Some(per_value.get(value).copied().unwrap_or(Inert));
        }
        Some(match value {
            0 if per_value.contains(&Object) => ArgReach::Nested,
            _ => Inert,
        })
    }
}

/// The printf-family layout of the builtin `name` (case-insensitive), or
/// `None` for any other function.
///
/// `fprintf` and `vfprintf` (format at position 1) are not listed: the catalog
/// has no row for either yet, and a layout without a row would read a call the
/// effect lane still cannot place.
#[must_use]
pub fn printf_family(name: &str) -> Option<PrintfFamily> {
    let key = name.trim_start_matches('\\');
    let at = |format, vector| Some(PrintfFamily { format, vector });
    match key.to_ascii_lowercase().as_str() {
        "sprintf" | "printf" => at(0, false),
        "vsprintf" | "vprintf" => at(0, true),
        _ => None,
    }
}

/// The largest value position a format may name before [`format_reach`] gives
/// up: far past any call's argument count, and a bound on the answer's size.
const MAX_FORMAT_POSITIONS: usize = 1024;

/// What one parse of a literal printf format answers ([`read_format`]): which
/// user code each value reaches, and whether any conversion reads the locale.
///
/// Both verdicts come from the same walk of the bytes, so they cannot disagree
/// about which specs the format holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatReading {
    /// One [`ArgReach`] per value position, as [`format_reach`] answers it.
    pub reach: Vec<ArgReach>,
    /// Whether some conversion reads the locale's decimal point: `f`, `g` and
    /// `G` do (ADR-0101 §3.1). `F`, `e`, `E`, `h`, `H`, every integer and
    /// character conversion, `s` and `%%` never do.
    ///
    /// `false` means no **locale** read, not that the call reads no setting.
    /// A `%s` of a value that may be a float renders it through the `precision`
    /// ini (`ini_set('precision', '3')` turns `1234.5678` into `1.23E+3`), and
    /// it is the only conversion that does: `d`, `e`, `F`, `g`, `h` and the rest
    /// give the same text at any `precision`, and none reads
    /// `serialize_precision` (witnessed, PHP 8.5, with variable arguments, since
    /// 8.4 folds a literal `sprintf('%s', 1.5)` at compile time). The `precision`
    /// cell has no label yet (ADR-0101 D4), so a caller that drops the locale
    /// label on `!reads_locale` must keep whatever read of `precision` the
    /// call's `%s` positions carry once that label exists.
    pub reads_locale: bool,
}

/// What a literal printf `format` does with each value it is given: one
/// [`ArgReach`] per value position, `0` being the first value after the
/// format. [`ArgReach::Object`] where some conversion naming the value is
/// `%s` (it renders the value, which runs an object's `__toString`) and
/// [`ArgReach::Inert`] where every conversion naming it is numeric (`d u c o
/// x X b e E f F g G h H`: an object becomes a number with a warning and runs
/// nothing). A value several conversions name takes the strongest of them. The
/// answer is as long as the highest position the format names; a value past it
/// is never read.
///
/// `None` is "the format cannot be read, ask the row": any spec the engine
/// would reject (an unknown conversion, a missing one at the end, a padding
/// quote with nothing after it, an argument number of zero or past `INT_MAX`),
/// a `*` width or precision (it consumes an argument as an integer and changes
/// which value the conversion names), a `%` with modifiers in front of it (a
/// conversion that renders `%` and still consumes a value slot), and a format
/// naming a position past 1,024 (far past any call's argument count). A
/// malformed format may still run `__toString` for an earlier `%s` before it
/// fails, so the whole format is unreadable, never a prefix of it.
///
/// The grammar is php-src's `php_formatted_print`
/// (`ext/standard/formatted_print.c`), read as bytes: `%%`, then a spec that
/// starts with a letter (just the conversion) or otherwise with an optional
/// `n$`, any of the flags `' '`, `0`, `-`, `+`, `'c` (c any byte), a width, a
/// `.` precision, an optional `l`, and the conversion. A spec with no `n$` takes
/// the next value in order; a spec with one leaves that counter alone.
#[must_use]
pub fn format_reach(format: &str) -> Option<Vec<ArgReach>> {
    read_format(format).map(|reading| reading.reach)
}

/// [`format_reach`]'s parse with its second verdict, whether the format reads
/// the locale (ADR-0101 §3.1). `None` is the same "unreadable" as there, and an
/// unreadable format keeps the read: [`format_reads_locale`] answers `true` for
/// it. The grammar is php-src's `php_formatted_print`, described on
/// [`format_reach`].
///
/// The locale verdict is over the whole format, never a prefix: a malformed
/// format may have rendered an earlier spec before it fails, and `%%f` is no
/// spec at all.
#[must_use]
pub fn read_format(format: &str) -> Option<FormatReading> {
    let bytes = format.as_bytes();
    let mut reach: Vec<ArgReach> = Vec::new();
    let mut reads_locale = false;
    let mut next = 0_usize;
    let mut at = 0_usize;
    while let Some(offset) = bytes[at..].iter().position(|&b| b == b'%') {
        at += offset + 1;
        if bytes.get(at) == Some(&b'%') {
            at += 1;
            continue;
        }
        let (position, kind, locale) = spec(bytes, &mut at, &mut next)?;
        if position >= MAX_FORMAT_POSITIONS {
            return None;
        }
        if reach.len() <= position {
            reach.resize(position + 1, Inert);
        }
        reach[position] = reach[position].max(kind);
        reads_locale |= locale;
    }
    Some(FormatReading { reach, reads_locale })
}

/// Whether a printf-family call with this literal `format` reads the locale:
/// `true` when a conversion ends in `f`, `g` or `G`, and `true` for a format
/// the parser cannot read as the engine does, since the row's read stands
/// unless a format shows it away. `false` is the proof that none does.
#[must_use]
pub fn format_reads_locale(format: &str) -> bool {
    read_format(format).is_none_or(|reading| reading.reads_locale)
}

/// One conversion spec of [`read_format`], `at` just past its `%`: the value
/// position it names, the reach of its conversion and whether the conversion
/// reads the locale, leaving `at` past it.
fn spec(bytes: &[u8], at: &mut usize, next: &mut usize) -> Option<(usize, ArgReach, bool)> {
    let mut named = None;
    if !bytes.get(*at).is_some_and(u8::is_ascii_alphabetic) {
        let digits = digit_run(bytes, *at);
        if bytes.get(*at + digits) == Some(&b'$') {
            named = Some(number(&bytes[*at..*at + digits]).filter(|&n| n > 0)? - 1);
            *at += digits + 1;
        }
        flags(bytes, at)?;
        if bytes.get(*at) == Some(&b'*') {
            return None;
        }
        skip_number(bytes, at)?;
        if bytes.get(*at) == Some(&b'.') {
            *at += 1;
            if bytes.get(*at) == Some(&b'*') {
                return None;
            }
            skip_number(bytes, at)?;
        }
    }
    if bytes.get(*at) == Some(&b'l') {
        *at += 1;
    }
    let position = named.unwrap_or_else(|| {
        *next += 1;
        *next - 1
    });
    let conversion = *bytes.get(*at)?;
    let kind = match conversion {
        b's' => Object,
        b'd' | b'u' | b'c' | b'o' | b'x' | b'X' | b'b' | b'e' | b'E' | b'f' | b'F' | b'g'
        | b'G' | b'h' | b'H' => Inert,
        _ => return None,
    };
    *at += 1;
    // `php_sprintf_appenddouble` hands `php_conv_fp` the locale's decimal point
    // for `f`, and its `g`/`G` arm overrides `.` with it; `F`, `e`, `E`, `h`
    // and `H` never consult it.
    Some((position, kind, matches!(conversion, b'f' | b'g' | b'G')))
}

/// The length of the ASCII digit run at `at`.
fn digit_run(bytes: &[u8], at: usize) -> usize {
    bytes[at.min(bytes.len())..].iter().take_while(|b| b.is_ascii_digit()).count()
}

/// The value of an ASCII digit run, as php-src's `php_sprintf_getnumber`
/// reads it: `None` for an empty run or one at or past `INT_MAX`, which the
/// engine rejects.
fn number(digits: &[u8]) -> Option<usize> {
    const INT_MAX: u64 = 2_147_483_647;
    if digits.is_empty() {
        return None;
    }
    let value = digits
        .iter()
        .fold(0_u64, |n, d| n.saturating_mul(10).saturating_add(u64::from(d - b'0')));
    if value >= INT_MAX { None } else { usize::try_from(value).ok() }
}

/// Moves `at` past a width or precision's digits, if there are any.
fn skip_number(bytes: &[u8], at: &mut usize) -> Option<()> {
    let digits = digit_run(bytes, *at);
    if digits > 0 {
        number(&bytes[*at..*at + digits])?;
        *at += digits;
    }
    Some(())
}

/// Moves `at` past a spec's flags: `' '`, `0`, `-`, `+`, and `'` with the
/// padding byte after it, which must exist.
fn flags(bytes: &[u8], at: &mut usize) -> Option<()> {
    loop {
        match bytes.get(*at) {
            Some(b' ' | b'0' | b'-' | b'+') => *at += 1,
            Some(b'\'') => {
                bytes.get(*at + 1)?;
                *at += 2;
            }
            _ => return Some(()),
        }
    }
}

/// The curated rows of `name`.
fn overrides(name: &str) -> &'static [(usize, ArgReach)] {
    OVERRIDES.iter().find(|(n, _)| *n == name).map_or(&[], |(_, row)| row)
}

use ArgReach::{Autoload, Callback, Inert, Object};

/// Where php-src does less with a parameter than its declared type admits,
/// or where the derivation cannot see what it does. Each row is witnessed in
/// both calling modes on PHP 8.5.11; positions are 0-based.
const OVERRIDES: &[(&str, &[(usize, ArgReach)])] = &[
    // A `mixed` value read only for its type tag, or converted by a handler
    // that has no userland hook: a userland object converts to `int` or
    // `float` with a warning and to `bool` as `true`, without `__toString`.
    ("boolval", &[(0, Inert)]),
    ("doubleval", &[(0, Inert)]),
    ("floatval", &[(0, Inert)]),
    ("gettype", &[(0, Inert)]),
    ("intval", &[(0, Inert)]),
    // A value stored or copied, never converted or compared.
    ("array_fill", &[(2, Inert)]),
    ("array_filter", &[(0, Inert)]),
    ("array_merge", &[(0, Inert)]),
    ("array_pop", &[(0, Inert)]),
    ("array_push", &[(0, Inert), (1, Inert)]),
    ("array_shift", &[(0, Inert)]),
    ("shuffle", &[(0, Inert)]),
    ("array_unshift", &[(0, Inert), (1, Inert)]),
    // `array_splice` converts a non-array replacement to an array. Kept at
    // `Object`, the reading that rules out least, although no user code was
    // seen to run there (a lazy ghost stays uninitialized).
    ("array_splice", &[(0, Inert), (3, Object)]),
    // `count` calls `Countable::count` on an object and never looks inside an
    // array, `COUNT_RECURSIVE` included.
    ("count", &[(0, Object)]),
    ("sizeof", &[(0, Object)]),
    // A rendered value: an object runs `__toString`, an array renders as
    // `Array` without its elements being read.
    ("printf", &[(1, Object)]),
    ("sprintf", &[(1, Object)]),
    ("strval", &[(0, Object)]),
    ("settype", &[(0, Object)]),
    // An internal enum, which userland cannot extend.
    ("round", &[(2, Inert)]),
    // The key sorts compare keys, which are never objects; the comparator
    // sorts hand the values to the callback and compare nothing themselves.
    ("krsort", &[(0, Inert)]),
    ("ksort", &[(0, Inert)]),
    ("uasort", &[(0, Inert)]),
    ("uksort", &[(0, Inert)]),
    ("usort", &[(0, Inert)]),
    // The internal pointer moves read an object's property table, which runs
    // a lazy object's initializer; an array is not read.
    ("end", &[(0, Object)]),
    ("next", &[(0, Object)]),
    ("prev", &[(0, Object)]),
    ("reset", &[(0, Object)]),
    // `array_walk` hands values to its callback; an object's property table
    // is read, as above. Its extra argument goes to the callback untouched.
    ("array_walk", &[(0, Object), (2, Inert)]),
    ("array_walk_recursive", &[(0, Object), (2, Inert)]),
    // The invokers hand values to their callback without converting them.
    ("array_all", &[(0, Inert)]),
    ("array_any", &[(0, Inert)]),
    ("array_find", &[(0, Inert)]),
    ("array_find_key", &[(0, Inert)]),
    ("array_map", &[(1, Inert), (2, Inert)]),
    ("array_reduce", &[(0, Inert), (2, Inert)]),
    ("call_user_func", &[(1, Inert)]),
    ("call_user_func_array", &[(1, Inert)]),
    ("iterator_apply", &[(2, Inert)]),
    ("register_shutdown_function", &[(1, Inert)]),
    // The values of `preg_replace_callback_array`'s map are callables.
    ("preg_replace_callback_array", &[(0, Callback)]),
    // `is_callable` resolves a class named in a string or array.
    ("is_callable", &[(0, Autoload), (2, Inert)]),
    // A user stream wrapper or filter is attributed to its registration, as an
    // error handler is (ADR-0099 §4.5): the class named at the registration (and
    // the filter name that attaches a registered user filter to a stream) is user
    // code that later runs inside I/O calls, and is never ruled out here.
    ("stream_wrapper_register", &[(1, Autoload)]),
    ("stream_filter_register", &[(1, Autoload)]),
    ("stream_filter_append", &[(1, Autoload)]),
    ("stream_filter_prepend", &[(1, Autoload)]),
    // By-reference out-parameters whose incoming value is discarded unread.
    ("preg_match", &[(2, Inert)]),
    ("preg_match_all", &[(2, Inert)]),
    ("preg_replace", &[(4, Inert)]),
    ("preg_replace_callback", &[(4, Inert)]),
    ("similar_text", &[(2, Inert)]),
    ("str_ireplace", &[(3, Inert)]),
    ("str_replace", &[(3, Inert)]),
];

#[cfg(test)]
mod tests {
    use super::{
        ArgReach, OVERRIDES, arg_reach, format_reach, format_reads_locale, printf_family,
        read_format,
    };
    use crate::{certified_at_call_site, effect_labels, foldable, knows, param_facts, throws_of};

    const I: ArgReach = ArgReach::Inert;
    const O: ArgReach = ArgReach::Object;

    fn at(name: &str, position: usize) -> ArgReach {
        arg_reach(name).unwrap_or_else(|| panic!("{name} has no row")).at(position)
    }

    #[test]
    fn the_derivation_reads_the_declared_type() {
        assert_eq!(at("strlen", 0), ArgReach::Coerced);
        assert_eq!(at("range", 0), ArgReach::Coerced, "string|int|float coerces an object");
        assert_eq!(at("intdiv", 0), ArgReach::Inert);
        assert_eq!(at("in_array", 0), ArgReach::Nested);
        assert_eq!(at("in_array", 1), ArgReach::Nested);
        assert_eq!(at("in_array", 2), ArgReach::Inert);
        assert_eq!(at("implode", 0), ArgReach::Nested, "array|string takes the stronger member");
        assert_eq!(at("json_encode", 0), ArgReach::Nested);
        assert_eq!(at("array_filter", 1), ArgReach::Callback);
        assert_eq!(at("iterator_apply", 0), ArgReach::Object, "a Traversable runs its methods");
        assert_eq!(at("fwrite", 0), ArgReach::Inert, "a resource position");
    }

    #[test]
    fn the_overrides_hold_where_php_src_does_less() {
        assert_eq!(at("count", 0), ArgReach::Object, "a Countable, never an array's elements");
        assert_eq!(at("gettype", 0), ArgReach::Inert);
        assert_eq!(at("strval", 0), ArgReach::Object);
        assert_eq!(at("is_callable", 0), ArgReach::Autoload);
        assert_eq!(at("preg_match", 2), ArgReach::Inert, "an out-parameter is written, not read");
        assert_eq!(at("usort", 0), ArgReach::Inert, "the comparator sees the values");
        assert_eq!(at("ksort", 0), ArgReach::Inert);
        assert_eq!(at("shuffle", 0), ArgReach::Inert, "a permutation compares nothing");
        assert_eq!(at("sort", 0), ArgReach::Nested);
    }

    /// Every curated row names a mined function and positions it has.
    #[test]
    fn every_override_names_a_mined_position() {
        for (name, row) in OVERRIDES {
            let facts = param_facts(name).unwrap_or_else(|| panic!("{name} was not mined"));
            for (position, _) in *row {
                assert!(*position < facts.params.len(), "{name} has no position {position}");
            }
        }
    }

    #[test]
    fn a_variadic_tail_repeats_and_a_position_past_the_list_is_inert() {
        assert_eq!(at("sprintf", 5), ArgReach::Object);
        assert_eq!(at("array_merge", 7), ArgReach::Inert);
        assert_eq!(at("strlen", 3), ArgReach::Inert, "an ArgumentCountError runs nothing");
    }

    /// A registration that lets user code run inside later I/O calls reaches that
    /// code at the registration, in every calling mode and for every argument
    /// (ADR-0099 §4.5); none of them is a throwless name.
    #[test]
    fn a_stream_wrapper_or_filter_registration_reaches_user_code() {
        let registrations = [
            "stream_wrapper_register",
            "stream_filter_register",
            "stream_filter_append",
            "stream_filter_prepend",
        ];
        for name in registrations {
            assert_eq!(at(name, 1), ArgReach::Autoload, "{name}");
            let row = arg_reach(name).expect(name);
            assert!(row.reaches_blind(true), "{name} reaches even in strict mode");
            assert_eq!(crate::throws_of(name), None, "{name} is not throwless");
        }
    }

    // ---- the printf-format parser (issue #860, S7-catalog) ----------------
    //
    // Every verdict below is checked against PHP 8.5.11: a probe passed one
    // tagged object per value and logged which `__toString` ran.

    /// The numeric conversions witnessed as inert (row 7.9): an object becomes a
    /// number with a warning, flags, padding quotes, widths, precisions and the
    /// `l` modifier included; `%%` names no value.
    #[test]
    fn a_numeric_conversion_never_reaches_user_code() {
        let cases: &[(&str, &[ArgReach])] = &[
            ("%d", &[I]),
            ("%.2f", &[I]),
            ("%05d", &[I]),
            ("100%% %d", &[I]),
            ("%u %o", &[I, I]),
            ("%lx %ld", &[I, I]),
            ("%h %H %g %G %e %E %F", &[I; 7]),
            ("%c%c%d", &[I, I, I]),
            ("%b%X", &[I, I]),
        ];
        for (format, expected) in cases {
            assert_eq!(format_reach(format).as_deref(), Some(*expected), "{format}");
        }
    }

    /// Only `%s` renders its value (rows 7.2 to 7.6 and 7.9): the value lands on
    /// the conversion that names it, in order or by `n$`, whatever flags,
    /// padding quote, width and precision sit in front.
    #[test]
    fn a_string_conversion_reaches_the_value_it_names() {
        let cases: &[(&str, &[ArgReach])] = &[
            ("%s", &[O]),             // 7.2
            ("%d %s", &[I, O]),       // 7.3
            ("%s %d", &[O, I]),       // 7.4
            ("%2$s %1$d", &[I, O]),   // 7.5
            ("%2$d %1$s", &[O, I]),   // 7.6
            ("%5.2s", &[O]),
            ("% 5s", &[O]),
            ("%ls", &[O]),
            ("%-+ 05.3s", &[O]),
            ("%5.s", &[O]),
            ("%.s", &[O]),
            ("%1$-5s", &[O]),
            ("%1$'#5s", &[O]),
            ("%'xs", &[O]), // the padding byte is `x`, the conversion `s`
            ("%1$05d %2$s", &[I, O]),
            ("%c%c%s", &[I, I, O]),
            ("%%%s", &[O]),
            ("%s%%", &[O]),
            ("%3$s", &[I, I, O]),
            // 7.9: the `'*` padding, `-5s`, `+.1e`, `%x`, `%c`, `%u` and `%b`.
            ("%'*10d|%-5s|%+.1e|%x|%c|%u|%b", &[I, O, I, I, I, I, I]),
        ];
        for (format, expected) in cases {
            assert_eq!(format_reach(format).as_deref(), Some(*expected), "{format}");
        }
    }

    /// A value several conversions name takes the strongest of them (row 7.7:
    /// `%1$d|%1$s` runs `__toString` for the `%s`), and a spec with `n$` leaves
    /// the in-order counter alone.
    #[test]
    fn a_value_named_twice_reaches_if_any_conversion_renders_it() {
        assert_eq!(format_reach("%1$d|%1$s").as_deref(), Some(&[O][..]));
        assert_eq!(format_reach("%1$s %1$d").as_deref(), Some(&[O][..]));
        assert_eq!(format_reach("%1$s%2$s%1$s").as_deref(), Some(&[O, O][..]));
        // `%2$s` does not advance the counter: the two plain `%s` take values 0 and 1.
        assert_eq!(format_reach("%2$s %s %s").as_deref(), Some(&[O, O][..]));
        assert_eq!(format_reach("%s %2$s %s").as_deref(), Some(&[O, O][..]));
        assert_eq!(format_reach("%1$d %d %s").as_deref(), Some(&[I, O][..]));
    }

    /// A format with no conversion names no value, and text between conversions
    /// is just text.
    #[test]
    fn a_format_without_conversions_names_nothing() {
        for format in ["", "abc", "100%%", "%%%%"] {
            assert_eq!(format_reach(format).as_deref(), Some(&[][..]), "{format:?}");
        }
    }

    /// Row 7.10 and the rest of what the engine rejects: the whole format is
    /// unreadable. `%s %q` runs `%s`'s `__toString` before the `ValueError`, so
    /// a prefix is never trusted.
    #[test]
    fn a_malformed_format_is_unreadable() {
        let malformed = [
            "%q",                // 7.10, ValueError: Unknown format specifier
            "%s %q",             // reached [0] before the ValueError
            "%d %q",             // ArgumentCountError first, still unreadable
            "%",                 // ValueError: Missing format specifier
            "%5.",               // ValueError
            "%1$",               // ValueError
            "%'",                // ValueError: Missing padding character
            "%'x",               // ValueError: no conversion after the padding
            "%0$s",              // ValueError: argument number must be positive
            "%$s",               // ValueError: no digits before `$`
            "%2147483647$s",     // ValueError: at INT_MAX
            "%99999999999999$s", // ValueError: past INT_MAX
            "%2147483647d",      // ValueError: Width must be between 0 and INT_MAX
            "%.2147483647d",     // ValueError: Precision
            "%-$s",              // `$` is not a conversion
            "% 1$s",             // the argument number must come first
            "%\u{e9}",           // a multibyte letter is no conversion
            "%1025$s",           // past the parser's own bound
        ];
        for format in malformed {
            assert_eq!(format_reach(format), None, "{format:?}");
        }
    }

    /// `*` takes its width or precision from an argument and shifts which value
    /// the conversion names, and `%` behind modifiers renders `%` while
    /// consuming a value slot (`sprintf('%5%s', $a, $o)` never reaches `$o`):
    /// the parser models neither, so the format is left to the row.
    #[test]
    fn a_star_or_a_modified_percent_is_left_to_the_row() {
        for format in ["%*d", "%.*f", "%s%*d", "%1$*d", "%5%s", "%-%", "%1$%"] {
            assert_eq!(format_reach(format), None, "{format:?}");
        }
    }

    /// A value count that does not match the format changes the outcome, never
    /// the parse: too few values is an `ArgumentCountError` (a `ValueError` for
    /// `vsprintf`, which its throw row records; row 7.11, neither runs user code),
    /// and a value past the last conversion is never read.
    #[test]
    fn the_parse_does_not_depend_on_how_many_values_are_given() {
        assert_eq!(format_reach("%d %d").as_deref(), Some(&[I, I][..])); // 7.11
        let sprintf = printf_family("sprintf").expect("sprintf");
        let parsed = format_reach("%d").expect("readable");
        assert_eq!(sprintf.reach_at(&parsed, 1), Some(I)); // `sprintf('%d', $o)`
        assert_eq!(sprintf.reach_at(&parsed, 2), Some(I), "sprintf('%d', 1, $o) never reads $o");
        assert_eq!(sprintf.reach_at(&parsed, 7), Some(I));
        let parsed = format_reach("%s").expect("readable");
        assert_eq!(sprintf.reach_at(&parsed, 1), Some(O)); // 7.2
        assert_eq!(sprintf.reach_at(&parsed, 2), Some(I), "sprintf('%s', 1, $o) never reads $o");
    }

    /// Which functions are printf-family, where the format sits and how the
    /// values come: `fprintf` and `vfprintf` have no row yet and are not named
    /// (row 7.16, a follow-up).
    #[test]
    fn the_printf_family_names_its_format_and_whether_values_are_a_vector() {
        for (name, vector) in
            [("sprintf", false), ("printf", false), ("vsprintf", true), ("vprintf", true)]
        {
            let family = printf_family(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!((family.format_position(), family.is_vector()), (0, vector), "{name}");
        }
        assert_eq!(printf_family("\\SPrintF"), printf_family("sprintf"));
        for name in ["fprintf", "vfprintf", "number_format", "implode", "in_array", "sprintf2"] {
            assert_eq!(printf_family(name), None, "{name}");
        }
    }

    /// The format and anything before it is the row's: `reach_at` answers only
    /// the value positions.
    #[test]
    fn the_format_position_itself_is_left_to_the_row() {
        let sprintf = printf_family("sprintf").expect("sprintf");
        let parsed = format_reach("%s").expect("readable");
        assert_eq!(sprintf.reach_at(&parsed, 0), None);
        assert_eq!(at("sprintf", 0), ArgReach::Coerced, "a non-literal format may be an object");
    }

    /// Row 7.15: `vsprintf`'s array is inert when no conversion is `%s`
    /// (`vsprintf('%d-%d', $a)` printed `1-1`) and nested when one is
    /// (`vsprintf('%s', [$o])` ran `__toString`, as did `%s %d` and `%d %s`).
    #[test]
    fn a_vector_is_inert_unless_a_conversion_renders_an_element() {
        for name in ["vsprintf", "vprintf"] {
            let family = printf_family(name).expect(name);
            let numeric = format_reach("%d-%d").expect("readable");
            assert_eq!(family.reach_at(&numeric, 1), Some(I), "{name}");
            let string = format_reach("%s").expect("readable");
            assert_eq!(family.reach_at(&string, 1), Some(ArgReach::Nested), "{name}");
            let mixed = format_reach("%d %s").expect("readable");
            assert_eq!(family.reach_at(&mixed, 1), Some(ArgReach::Nested), "{name}");
            assert_eq!(family.reach_at(&mixed, 2), Some(I), "{name}: past the array");
            assert_eq!(family.reach_at(&mixed, 0), None, "{name}: the format");
        }
        // The row stays what the declared types say where no literal format helps.
        assert_eq!(at("vsprintf", 0), ArgReach::Coerced);
        assert_eq!(at("vsprintf", 1), ArgReach::Nested);
    }

    // ---- the effect row of `array_search` (row 7.12) ----------------------

    /// `array_search` answers `no-effect-row` on master: it is not foldable, and
    /// the call-site certification is what the effect lane asks next. It is
    /// certified at a call site and nowhere else, so no pass reads it as pure
    /// argument-blind, and the reach rule holds every call.
    #[test]
    fn array_search_is_certified_at_the_call_site() {
        assert!(certified_at_call_site("array_search") && certified_at_call_site("ARRAY_SEARCH"));
        assert!(knows("array_search"));
        assert!(effect_labels("array_search").is_none() && !foldable("array_search"));
        assert!(arg_reach("array_search").expect("row").reaches_blind(true));
    }

    /// `vsprintf` is not certified pure: `%f`, `%g` and `%G` read `LC_NUMERIC`
    /// (issue #991, as `sprintf`'s do), so it is not on the call-site list, and
    /// ADR-0101 gives it the same locale-read row as `sprintf`. Its reach
    /// answer is still right, and `printf_family` still names it.
    #[test]
    fn vsprintf_is_not_certified_but_keeps_its_reach() {
        assert!(!certified_at_call_site("vsprintf"));
        assert!(knows("vsprintf"));
        assert_eq!(effect_labels("vsprintf"), effect_labels("sprintf"));
        assert_eq!(effect_labels("vsprintf"), Some(&["global.read.setting.locale"][..]));
        assert!(printf_family("vsprintf").is_some());
    }

    /// The by-value reach of `array_search` is `in_array`'s: needle and
    /// haystack are `mixed` and `array` (`Nested`), the strict flag is a bool.
    /// Rows 7.13 and 7.14 stay with `in_array`.
    #[test]
    fn array_search_reaches_as_in_array_does() {
        for name in ["array_search", "in_array"] {
            assert_eq!(at(name, 0), ArgReach::Nested, "{name}");
            assert_eq!(at(name, 1), ArgReach::Nested, "{name}");
            assert_eq!(at(name, 2), ArgReach::Inert, "{name}");
            assert_eq!(at(name, 3), ArgReach::Inert, "{name}: past the list");
        }
    }

    /// Row 7.16: `fprintf` and `vfprintf` have no row of any kind, and this
    /// change gives them none.
    #[test]
    fn fprintf_and_vfprintf_keep_no_effect_row() {
        for name in ["fprintf", "vfprintf"] {
            assert!(!certified_at_call_site(name), "{name}");
            assert!(effect_labels(name).is_none() && !foldable(name), "{name}");
            assert_eq!(throws_of(name), None, "{name}");
        }
    }

    #[test]
    fn a_certified_name_reaches_nothing() {
        for name in ["is_int", "is_string", "get_debug_type", "array_values", "array_key_exists"] {
            assert!(!arg_reach(name).expect(name).reaches_blind(false), "{name}");
        }
    }

    #[test]
    fn an_unreadable_argument_list_reaches_what_strictness_leaves() {
        let strlen = arg_reach("strlen").expect("strlen");
        assert!(strlen.reaches_blind(false));
        assert!(!strlen.reaches_blind(true), "a coerced string parameter is a TypeError there");
        assert!(arg_reach("count").expect("count").reaches_blind(true));
    }

    /// The string family is certified at a call site only: no other pass knows
    /// it, and strictness alone rules out everything its arguments reach.
    #[test]
    fn the_string_family_reaches_only_through_coercion() {
        for name in [
            "strcmp", "strncmp", "strcasecmp", "strncasecmp", "strspn", "strcspn", "substr_count",
            "ord", "chr", "bin2hex", "hex2bin", "dirname", "unpack",
        ] {
            assert!(certified_at_call_site(name), "{name}");
            assert!(effect_labels(name).is_none() && !foldable(name), "{name} is known blind");
            assert!(!arg_reach(name).expect(name).reaches_blind(true), "{name}");
        }
        for name in ["basename", "strnatcasecmp", "substr_compare", "htmlspecialchars"] {
            assert!(!certified_at_call_site(name), "{name} reads the locale or an ini setting");
        }
    }

    // ---- the locale verdict of the same parse (ADR-0101 §3.1, issue #991) ---

    /// Which conversion letters read `LC_NUMERIC`: `f`, `g` and `G`. Every
    /// other letter php-src's formatter knows does not, and the verdict does
    /// not depend on flags, padding, width, precision, `n$` or `l`.
    #[test]
    fn only_f_g_and_capital_g_read_the_locale() {
        let modifiers = ["", "5", "-8", "+", "05", ".0", ".2", "1$", "1$.3", "'*8", "+010.4", "l"];
        for letter in ['f', 'g', 'G'] {
            for m in modifiers {
                let format = format!("%{m}{letter}");
                assert!(format_reads_locale(&format), "{format} reads the locale");
            }
        }
        for letter in ['F', 'e', 'E', 'h', 'H', 'd', 'u', 'c', 'o', 'x', 'X', 'b', 's'] {
            for m in modifiers {
                let format = format!("%{m}{letter}");
                assert!(!format_reads_locale(&format), "{format} does not read the locale");
            }
        }
    }

    /// The verdict is over the whole format: one reading conversion anywhere
    /// keeps the read, and text, `%%` and `%%f` (an escaped percent and a
    /// letter) are not conversions.
    #[test]
    fn the_locale_verdict_is_over_the_whole_format() {
        for format in ["%d %f", "%f %d", "%s-%d-%.2f-%s", "%%%f", "%1$s %1$.2f", "%d%%%G"] {
            assert!(format_reads_locale(format), "{format}");
        }
        for format in
            ["", "abc", "%d-%s", "%%f", "%%g%%G", "100%%", "%d%%f", "%s %d %F %e %E %h %H"]
        {
            assert!(!format_reads_locale(format), "{format:?}");
        }
    }

    /// A format the parser cannot read as the engine does keeps the read, and
    /// so does one whose readable prefix is clean: a malformed format may have
    /// rendered an earlier conversion before it fails.
    #[test]
    fn a_format_the_parser_cannot_read_keeps_the_read() {
        for format in ["%q", "%d %q", "%", "%'", "%0$f", "%*d", "%.*F", "%5%s", "%1025$d"] {
            assert_eq!(read_format(format), None, "{format:?}");
            assert!(format_reads_locale(format), "{format:?}");
        }
    }

    /// One parse gives both verdicts: `format_reach` is `read_format`'s reach
    /// on every format, readable or not, so the two cannot disagree about which
    /// specs they saw.
    #[test]
    fn one_parse_gives_both_verdicts() {
        for format in [
            "", "%d", "%s", "%.2f", "%1$s %1$d", "%2$s %s %s", "%d %q", "%*d", "%5%s", "100%%",
            "%'x5s", "%lf", "%H",
        ] {
            assert_eq!(format_reach(format), read_format(format).map(|r| r.reach), "{format:?}");
        }
        let both = read_format("%s %.1f").expect("readable");
        assert_eq!(both.reach, [O, I]);
        assert!(both.reads_locale);
    }
}
