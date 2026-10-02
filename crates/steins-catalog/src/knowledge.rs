//! What the catalog **knows** of a builtin function ([`knows`]) and what it
//! **throws** ([`throws_of`], [`flag_gated_throw`]) (ADR-0099 §3).
//!
//! The analysis asks two different questions about a spelling, and they used to
//! be answered by seven separate closures that each read a different table:
//!
//! * *is this a builtin at all?* — [`knows`], the union of every table that
//!   states something about a function. A project function that shadows a
//!   builtin spelling is ambiguous exactly when this says yes, so the answer
//!   must not depend on which pass asks. Two questions stay outside it: the
//!   shadow check (a user declaration that shadows a builtin, which answers
//!   "no catalog at all") and runtime-existence questions (`function_exists`
//!   folding, the absence family), which read their own tables.
//! * *what does it raise?* — [`throws_of`]. A row is the exception classes the
//!   call provably raises, an empty row is **audited** throwlessness, and
//!   `None` is *unknown*: the analysis reads that as a coverage gap, never as
//!   nothing (ADR-0099 §3.2).
//!
//! # The throwless table
//!
//! `THROWLESS_NAMES` lists the builtins php-src shows raise nothing for any argument
//! their parameter types admit. The fold allowlist and the certified-pure lists
//! are **candidates** for it, not evidence: they answer whether the engine may
//! evaluate a name or whether it has an effect, and `json_encode` is foldable
//! while it throws under `JSON_THROW_ON_ERROR`.
//!
//! Each name is audited, in two parts:
//!
//! 1. **A body reading.** The function's body at `PINNED_PHP` contains no
//!    `zend_throw_*`, `zend_value_error` or `zend_argument_value_error` outside
//!    argument parsing, and calls nothing that raises on a value.
//! 2. **A runtime witness** on PHP 8.5.11: 12,000 argument tuples per name,
//!    drawn from every parameter's admitted values (integer extremes, `NAN` and
//!    `INF`, empty and NUL-carrying strings, malformed encodings, nested arrays
//!    holding an array key of `PHP_INT_MAX`, `Countable`, `Stringable` and plain
//!    objects, closures, resources), each run with a swallowing error handler.
//!    The audit's rule: no `Throwable` other than an `ArgumentCountError` and a
//!    `TypeError` whose message is an argument check (`name(): Argument #N ($x)
//!    must be …`). A `TypeError` that depends on *which value* an admitted
//!    argument holds is not one: `array_column` raises it for an array key
//!    (`array_column([['a' => [1], 'b' => 1]], 'b', 'a')`), so it has a row.
//!
//! A third part is mechanical. `docs/research/phpsrc-mining/audit_throwless.py`
//! resolves every name on the table to the C function php-src runs for it
//! (through the generated `*_arginfo.h` tables, so an alias such as `join` is
//! `implode`'s, and through the `FileFunction` macro that defines `file_exists`
//! and its stat siblings), walks that function's call graph at the pinned
//! php-src, and lists each raise it can reach. Its output,
//! `docs/research/phpsrc-mining/throwless_audit.md`, classifies every name as
//! `none`, `argument-checking` or `destructor-hazard`, or `needs-row` for a name
//! that left the table, and a test below holds the table to it. The pin is
//! php-src master (8.6.0-dev) and this table is the 8.5 line's, so a raise that
//! exists only there is recorded as such in the note (`8.6:`), and not as a row:
//! `array_filter`'s `$mode` and `pathinfo`'s `$flags` are a `ValueError` on 8.6
//! and accepted on 8.5.11.
//!
//! **Argument checking is set aside**, as in ADR-0099 §3.3: a `TypeError` from a
//! parameter type, an unknown named parameter, a spread or arity mismatch, and
//! `array_key_exists`'s `TypeError` for an array or object key (its stub types
//! the key `mixed`) are the same for every builtin, and the throw lane does not
//! model them. So is the conversion of an *object* argument to a string (`Object
//! of class X could not be converted to string`, an `Error`): it needs an
//! object operand, and every argument position that can hold one is a
//! `Coerced`, `Object` or `Nested` reach ([`crate::arg_reach`]), which the
//! resolver turns into a coverage gap of its own unless the call site rules the
//! object out. A name whose `Error` needs no object is not on the table:
//! `array_push` and `array_merge_recursive` raise `Error` when a key is
//! `PHP_INT_MAX` ("Cannot add element to the array as the next element is
//! already occupied") and `get_class()` raises it outside a class.
//! `call_user_func_array` is simply unaudited. An `array` parameter also admits an
//! array that contains itself by reference, and comparing two of them is an
//! `Error` ("Nesting level too deep - recursive dependency?"): `in_array`,
//! `array_search`, `array_keys`, `array_unique`, `sort`, `rsort`, `asort` and
//! `arsort` compare their elements and carry a row, as do `array_replace_recursive`
//! and `array_walk_recursive` ("Recursion detected"), and `date_create` and
//! `date_create_immutable`, which raise `Error` for a user subclass of
//! `DateTimeZone` that never ran the parent's constructor (issue #881).
//!
//! **User code that only registration can attach** is attributed to the
//! registration (ADR-0099 §4.5), not to the calls it later runs inside. A user
//! stream wrapper or filter makes `file_exists`, `is_dir`, `filesize`, `fwrite`,
//! `fseek`, `fclose` and the rest of the stat and handle calls on the table throw
//! whatever its methods throw; `stream_wrapper_register`,
//! `stream_filter_register`, `stream_filter_append` and `stream_filter_prepend`
//! carry an `Autoload` reach ([`crate::arg_reach`]) and no throw row, so the body
//! that registers one has the gap. For the same reason `gc_collect_cycles` is off
//! the table: it runs destructors.
//!
//! What the table deliberately leaves out, so each stays a gap:
//!
//! * a name that looks a class up by string (`class_exists`, `is_a`,
//!   `method_exists`, `is_callable`, `defined` with a class constant) can run an
//!   autoloader, and the autoloader is user code that may throw;
//! * the `mb_*` family (`ValueError` for an unknown encoding), `iconv`;
//! * the names that open, resolve or list a path (`fopen`, `file_get_contents`,
//!   `realpath`, `glob`, `scandir`: a path with a NUL byte is a `ValueError` for
//!   the `p` parameter kind), the stream reads that take a length (`fread` and
//!   `fgets` raise a `ValueError` for a length below one), `define` (a class
//!   constant name is a `ValueError`) and `constant` (an undefined one is an
//!   `Error`). The stat questions (`file_exists`, `is_dir`, `filesize`…) and the
//!   stream operations on a handle (`fwrite`, `fseek`, `rewind`…) are on the
//!   table: a NUL byte there answers `false`, and a closed handle is the
//!   `TypeError` that argument checking sets aside;
//! * `serialize` (`Exception` for a closure), and `number_format`, whose
//!   audit did not finish (the fuzz exhausts memory on a huge `$decimals`);
//! * every name nobody has audited yet. The table starts from the corpus's most
//!   frequent pure names and grows by audit, one witnessed name at a time.

use crate::builtins::{builtin_throws, invocation_shape, param_facts_mined};
use crate::effects::{
    by_value_arg, certified_at_call_site, effect_labels, out_params, pure_at_arity,
};

/// Whether the catalog **knows** `name` as a builtin function: it states
/// something about it in at least one of its tables (ADR-0099 §3.1).
///
/// The union of the effect colours ([`effect_labels`]), the by-ref
/// out-parameter rows ([`out_params`]), the by-value certification
/// ([`by_value_arg`], which includes the mined arginfo), the mined parameter
/// facts ([`param_facts_mined`]), the callback invocation shapes
/// ([`invocation_shape`]), and the two call-site certified lists
/// ([`pure_at_arity`], [`certified_at_call_site`]).
///
/// It says nothing about *what* the catalog knows: a known name can lack a row
/// on any one axis, and the lane that needs the axis reads that as a coverage
/// gap (ADR-0099 §3.2). Matching is case-insensitive.
#[must_use]
pub fn knows(name: &str) -> bool {
    // The mined arginfo lists every function of the mining build, so it answers
    // for nearly every real builtin in one lookup; the rest are the tables'
    // hand-kept names the build did not have.
    param_facts_mined(name)
        || effect_labels(name).is_some()
        || out_params(name).is_some()
        || by_value_arg(name, 0).is_some()
        || invocation_shape(name).is_some()
        || pure_at_arity(name, 1)
        || certified_at_call_site(name)
}

/// The exception classes a call to the builtin `name` raises, as the catalog can
/// state them without reading the call's arguments (ADR-0099 §3.2, §3.3):
///
/// * `Some(&[classes…])` is the [`builtin_throws`] row: the global class names
///   the call provably raises for some argument values.
/// * `Some(&[])` is **audited throwlessness**: the name is on `THROWLESS_NAMES`.
/// * `None` is *unknown*, and a name that throws only under a flag
///   ([`flag_gated_throw`]) is unknown here too. A lane reads it as a coverage
///   gap, never as throwless.
///
/// Matching is case-insensitive.
#[must_use]
pub fn throws_of(name: &str) -> Option<&'static [&'static str]> {
    if flag_gated_throw(name).is_some() {
        return None;
    }
    builtin_throws(name).or_else(|| {
        THROWLESS_NAMES.binary_search(&lowercase(name).as_str()).is_ok().then_some(&[][..])
    })
}

/// [`throws_of`] for a call whose every argument is a **flat literal** (a scalar,
/// or an array literal of scalars): the one question where a value the row
/// exists for cannot be passed.
///
/// A name in `REFERENCE_VALUE_ERROR` carries the row `["Error"]` because an
/// `array` parameter admits an array that contains itself by reference, and a
/// `DateTimeZone` parameter a subclass that never ran the parent's constructor.
/// No literal is either, so such a call raises nothing, and the answer is the
/// empty row. Any other name answers as [`throws_of`] does.
#[must_use]
pub fn throws_of_literals(name: &str) -> Option<&'static [&'static str]> {
    let row = throws_of(name);
    if row.is_some_and(|r| r == ["Error"])
        && REFERENCE_VALUE_ERROR.binary_search(&lowercase(name).as_str()).is_ok()
    {
        return Some(&[]);
    }
    row
}

/// The names whose throw row is the `Error` a value that no literal is raises:
/// two distinct arrays that contain themselves compared (`in_array`,
/// `array_search`, `array_keys`, `array_unique`, `sort`, `rsort`, `asort`,
/// `arsort`), a recursive array walked (`array_replace_recursive`,
/// `array_walk_recursive`) and an uninitialised `DateTimeZone` subclass
/// (`date_create`, `date_create_immutable`). In byte order.
const REFERENCE_VALUE_ERROR: &[&str] = &[
    "array_keys",
    "array_replace_recursive",
    "array_search",
    "array_unique",
    "array_walk_recursive",
    "arsort",
    "asort",
    "date_create",
    "date_create_immutable",
    "in_array",
    "rsort",
    "sort",
];

/// A builtin that raises **only when a flag argument asks it to**: `json_encode`
/// and `json_decode` throw `JsonException` under `JSON_THROW_ON_ERROR`, and
/// nothing otherwise (ADR-0099 §3.3).
///
/// A call's throw set is [`Self::base`] when its flags argument is absent or a
/// constant expression without [`Self::flag`]; with the flag, or with a flags
/// argument nobody can read, the call has a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagGatedThrow {
    /// The 0-based position of the flags parameter.
    pub position: usize,
    /// The bit whose presence makes the call raise [`Self::when_set`].
    pub flag: i64,
    /// What the call raises without the flag: the name's other, input-determined
    /// throws (`json_decode`'s `ValueError` for a depth out of range).
    pub base: &'static [&'static str],
    /// What the flag adds.
    pub when_set: &'static [&'static str],
}

/// The flag-gated throw of `name` (case-insensitive), if it has one.
#[must_use]
pub fn flag_gated_throw(name: &str) -> Option<FlagGatedThrow> {
    /// `JSON_THROW_ON_ERROR`, PHP 7.3+.
    const JSON_THROW_ON_ERROR: i64 = 4_194_304;
    const JSON_EXCEPTION: &[&str] = &["JsonException"];
    match lowercase(name).as_str() {
        // `json_encode(mixed $value, int $flags = 0, int $depth = 512)`. Witnessed
        // on PHP 8.5.11: with flags 0, an unencodable value (`NAN`, a malformed
        // UTF-8 string, a recursive array) and a depth of 0 or `PHP_INT_MAX` all
        // return `false`; with the flag each of them is a `JsonException`.
        "json_encode" => Some(FlagGatedThrow {
            position: 1,
            flag: JSON_THROW_ON_ERROR,
            base: &[],
            when_set: JSON_EXCEPTION,
        }),
        // `json_decode(string $json, ?bool $associative = null, int $depth = 512,
        // int $flags = 0)`: a depth outside `1..=INT_MAX` is a `ValueError`
        // whatever the flags say, and a syntax error is a `JsonException` under
        // the flag.
        "json_decode" => Some(FlagGatedThrow {
            position: 3,
            flag: JSON_THROW_ON_ERROR,
            base: builtin_throws("json_decode").unwrap_or(&[]),
            when_set: JSON_EXCEPTION,
        }),
        _ => None,
    }
}

fn lowercase(name: &str) -> String {
    name.trim_start_matches('\\').to_ascii_lowercase()
}

/// The builtins **audited as throwless** (see the module doc for the audit), in
/// byte order so the table can be binary-searched and a new name is one added
/// line. `the_throwless_table_is_sorted_lowercase_and_known` pins the order.
const THROWLESS_NAMES: &[&str] = &[
    "abs",
    "addcslashes",
    "addslashes",
    "array_all",
    "array_any",
    "array_count_values",
    "array_diff",
    "array_diff_key",
    "array_fill_keys",
    "array_filter",
    "array_find",
    "array_find_key",
    "array_first",
    "array_flip",
    "array_intersect",
    "array_intersect_key",
    "array_is_list",
    "array_key_exists",
    "array_key_first",
    "array_key_last",
    "array_last",
    "array_map",
    "array_merge",
    "array_pop",
    "array_product",
    "array_reduce",
    "array_replace",
    "array_reverse",
    "array_shift",
    "array_slice",
    "array_splice",
    "array_sum",
    "array_unshift",
    "array_values",
    "array_walk",
    "base64_decode",
    "base64_encode",
    "basename",
    "bin2hex",
    "bindec",
    "boolval",
    "call_user_func",
    "ceil",
    "checkdate",
    "chop",
    "chr",
    "cos",
    "crc32",
    "ctype_alnum",
    "ctype_alpha",
    "ctype_cntrl",
    "ctype_digit",
    "ctype_graph",
    "ctype_lower",
    "ctype_print",
    "ctype_punct",
    "ctype_space",
    "ctype_upper",
    "ctype_xdigit",
    "current",
    "date",
    "date_default_timezone_get",
    "date_default_timezone_set",
    "decbin",
    "dechex",
    "decoct",
    "deg2rad",
    "doubleval",
    "end",
    "error_clear_last",
    "error_get_last",
    "error_reporting",
    "exp",
    "extension_loaded",
    "fclose",
    "feof",
    "fflush",
    "file_exists",
    "filemtime",
    "filesize",
    "floatval",
    "floor",
    "fmod",
    "fputs",
    "fseek",
    "ftell",
    "function_exists",
    "fwrite",
    "get_debug_type",
    "get_object_vars",
    "getcwd",
    "getdate",
    "getenv",
    "gethostname",
    "getmypid",
    "getrandmax",
    "gettype",
    "gmdate",
    "gmmktime",
    "headers_sent",
    "hexdec",
    "hrtime",
    "html_entity_decode",
    "htmlentities",
    "htmlspecialchars",
    "htmlspecialchars_decode",
    "hypot",
    "idate",
    "implode",
    "ini_get",
    "ini_set",
    "intval",
    "is_array",
    "is_bool",
    "is_countable",
    "is_dir",
    "is_double",
    "is_executable",
    "is_file",
    "is_finite",
    "is_float",
    "is_infinite",
    "is_int",
    "is_integer",
    "is_iterable",
    "is_link",
    "is_long",
    "is_nan",
    "is_null",
    "is_numeric",
    "is_object",
    "is_readable",
    "is_resource",
    "is_scalar",
    "is_string",
    "is_writable",
    "join",
    "key",
    "key_exists",
    "krsort",
    "ksort",
    "lcfirst",
    "lcg_value",
    "levenshtein",
    "localtime",
    "ltrim",
    "md5",
    "memory_get_peak_usage",
    "memory_get_usage",
    "microtime",
    "mktime",
    "mt_getrandmax",
    "mt_srand",
    "natcasesort",
    "natsort",
    "next",
    "nl2br",
    "octdec",
    "ord",
    "pathinfo",
    "phpversion",
    "pi",
    "posix_getpid",
    "preg_grep",
    "preg_last_error",
    "preg_last_error_msg",
    "preg_quote",
    "preg_replace",
    "preg_replace_callback",
    "preg_split",
    "prev",
    "print_r",
    "quotemeta",
    "rand",
    "rawurldecode",
    "rawurlencode",
    "reset",
    "restore_error_handler",
    "restore_exception_handler",
    "rewind",
    "rtrim",
    "sha1",
    "shuffle",
    "similar_text",
    "sin",
    "soundex",
    "spl_object_hash",
    "spl_object_id",
    "sqrt",
    "srand",
    "str_contains",
    "str_ends_with",
    "str_ireplace",
    "str_replace",
    "str_starts_with",
    "strcasecmp",
    "strcmp",
    "strcspn",
    "stream_context_get_options",
    "stream_get_meta_data",
    "stripslashes",
    "stristr",
    "strlen",
    "strnatcasecmp",
    "strnatcmp",
    "strrchr",
    "strrev",
    "strspn",
    "strstr",
    "strtolower",
    "strtotime",
    "strtoupper",
    "strtr",
    "strval",
    "substr",
    "substr_replace",
    "sys_get_temp_dir",
    "time",
    "tmpfile",
    "trim",
    "uasort",
    "ucfirst",
    "ucwords",
    "uksort",
    "umask",
    "uniqid",
    "urldecode",
    "urlencode",
    "usort",
    "var_export",
];

#[cfg(test)]
mod tests {
    use super::{THROWLESS_NAMES, flag_gated_throw, knows, throws_of};
    use crate::{builtin_throws, certified_at_call_site, effect_labels, out_params};

    #[test]
    fn knows_is_the_union_of_every_table_the_closures_read() {
        // Each table's own names, one per table.
        assert!(knows("strlen"), "an effect colour (foldable)");
        assert!(knows("file_put_contents"), "an effect colour");
        assert!(knows("preg_match"), "an out-parameter row");
        assert!(knows("sscanf"), "mined arginfo alone: no colour, no row, no certification");
        assert!(effect_labels("sscanf").is_none() && out_params("sscanf").is_none());
        assert!(knows("array_map"), "an invocation shape");
        assert!(knows("array_keys"), "certified pure at one argument");
        assert!(effect_labels("array_keys").is_none());
        for name in ["strcmp", "dirname", "unpack", "ord"] {
            assert!(knows(name), "{name}: certified at the call site");
            assert!(certified_at_call_site(name));
        }
        assert!(knows("trim"), "by-value certification");
        assert!(knows("STRLEN") && knows("ARRAY_KEYS"), "case-insensitive");
    }

    #[test]
    fn knows_rejects_what_no_table_states() {
        for name in ["", "not_a_builtin", "my_project_helper", "App\\strlen"] {
            assert!(!knows(name), "{name:?}");
        }
    }

    #[test]
    fn throws_of_reads_the_row_then_the_audited_table_and_otherwise_nothing() {
        // A row.
        assert_eq!(throws_of("intdiv"), Some(&["DivisionByZeroError", "ArithmeticError"][..]));
        assert_eq!(throws_of("DIRNAME"), Some(&["ValueError"][..]), "levels < 1");
        assert_eq!(throws_of("array_column"), Some(&["TypeError"][..]), "an array row value");
        // Audited throwless: an empty row, not a missing one.
        assert_eq!(throws_of("strlen"), Some(&[][..]));
        assert_eq!(throws_of("ARRAY_FILTER"), Some(&[][..]));
        assert_eq!(throws_of("\\strtolower"), Some(&[][..]));
        // A row the audit added: recursive arrays compare to an `Error`.
        assert_eq!(throws_of("ARRAY_KEYS"), Some(&["Error"][..]), "search form; see function_throws");
        assert_eq!(throws_of("in_array"), Some(&["Error"][..]));
        assert_eq!(throws_of("date_create"), Some(&["Error"][..]), "an uninitialised tz subclass");
        // Known, unaudited: unknown. `strlen` has a colour and `file_put_contents`
        // too, and neither row says what the other does not.
        assert_eq!(throws_of("file_put_contents"), Some(&["ValueError"][..]), "a path it refuses");
        assert_eq!(throws_of("curl_exec"), None);
        assert_eq!(throws_of("class_exists"), None, "autoloads");
        assert_eq!(throws_of("serialize"), None);
        assert_eq!(throws_of("not_a_builtin"), None);
    }

    #[test]
    fn a_literal_call_cannot_pass_the_value_a_reference_error_needs() {
        use super::{REFERENCE_VALUE_ERROR, throws_of_literals};
        assert!(REFERENCE_VALUE_ERROR.windows(2).all(|w| w[0] < w[1]), "sorted, no duplicates");
        for name in REFERENCE_VALUE_ERROR {
            assert_eq!(throws_of(name), Some(&["Error"][..]), "{name}: the row");
            assert_eq!(throws_of_literals(name), Some(&[][..]), "{name}: over literals");
        }
        assert_eq!(throws_of_literals("IN_ARRAY"), Some(&[][..]), "case-insensitive");
        // Every other answer is `throws_of`'s.
        for name in ["intdiv", "dirname", "strlen", "json_decode", "class_exists", "ksort"] {
            assert_eq!(throws_of_literals(name), throws_of(name), "{name}");
        }
    }

    #[test]
    fn a_flag_gated_name_has_no_flag_blind_row() {
        for name in ["json_encode", "JSON_DECODE"] {
            assert_eq!(throws_of(name), None, "{name} throws only under its flag");
        }
        let encode = flag_gated_throw("json_encode").expect("gated");
        assert_eq!((encode.position, encode.flag), (1, 4_194_304));
        assert!(encode.base.is_empty() && encode.when_set == ["JsonException"]);
        let decode = flag_gated_throw("json_decode").expect("gated");
        assert_eq!(decode.position, 3);
        assert_eq!(decode.base, ["ValueError"], "the depth check holds whatever the flags say");
        assert_eq!(flag_gated_throw("strlen"), None);
    }

    #[test]
    fn the_throwless_table_is_sorted_lowercase_and_known() {
        assert!(THROWLESS_NAMES.windows(2).all(|w| w[0] < w[1]), "sorted, no duplicates");
        for name in THROWLESS_NAMES {
            assert_eq!(*name, name.to_ascii_lowercase());
            assert!(knows(name), "{name} is audited, so the catalog must know it");
            assert!(builtin_throws(name).is_none(), "{name} has a throw row, so it throws");
            assert!(flag_gated_throw(name).is_none(), "{name} is flag-gated");
        }
    }

    /// One row of the generated audit note's table.
    struct AuditRow {
        name: String,
        on_table: bool,
        class: String,
    }

    /// The table of `docs/research/phpsrc-mining/throwless_audit.md`: the lines
    /// that open with a backticked name, split into their cells.
    fn audit_rows() -> Vec<AuditRow> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/research/phpsrc-mining/throwless_audit.md");
        let note =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        note.lines()
            .filter(|l| l.starts_with("| `"))
            .map(|l| {
                let cells: Vec<&str> = l.trim_matches('|').split('|').map(str::trim).collect();
                assert!(cells.len() >= 4, "audit table row: {l}");
                AuditRow {
                    name: cells[0].trim_matches('`').to_owned(),
                    on_table: cells[1] == "yes",
                    class: cells[3].to_owned(),
                }
            })
            .collect()
    }

    /// The table is held to the generated audit (issue #881): every name on it is
    /// in the note with a class that raises nothing for an admitted value, and a
    /// name the audit found raising one (`needs-row`) is off the table and has a
    /// throw row. The note is regenerated by `audit_throwless.py`, and a re-pin or
    /// a new name shows up here as a missing or unreviewed row.
    #[test]
    fn the_table_is_what_the_audit_classes_as_raising_nothing_for_an_admitted_value() {
        const CLEAN: [&str; 3] = ["argument-checking", "destructor-hazard", "none"];
        let rows = audit_rows();
        for name in THROWLESS_NAMES {
            let row = rows
                .iter()
                .find(|r| r.name == *name)
                .unwrap_or_else(|| panic!("{name} is on the table and missing from the audit"));
            assert!(row.on_table, "{name}: the note says it is off the table");
            assert!(CLEAN.contains(&row.class.as_str()), "{name}: audited as {}", row.class);
        }
        for row in &rows {
            let listed = THROWLESS_NAMES.contains(&row.name.as_str());
            assert_eq!(row.on_table, listed, "{}: the note and the table disagree", row.name);
            if row.class == "needs-row" {
                assert!(!listed, "{} raises for an admitted value", row.name);
                assert!(builtin_throws(&row.name).is_some(), "{}: needs-row has no row", row.name);
            } else {
                assert!(CLEAN.contains(&row.class.as_str()), "{}: {}", row.name, row.class);
            }
        }
        assert_eq!(rows.iter().filter(|r| r.on_table).count(), THROWLESS_NAMES.len());
    }

    /// The names an audit refused, so a later edit that adds one back meets the
    /// reason here. Each throws for an input its parameter types admit.
    #[test]
    fn what_the_audit_refused_stays_off_the_table() {
        let refused = [
            ("array_push", "Error: next index already occupied"),
            ("array_merge_recursive", "Error: next index already occupied"),
            ("get_class", "Error outside a class"),
            ("call_user_func_array", "Error: unknown named parameter"),
            ("serialize", "Exception for a closure"),
            ("class_exists", "autoloads"),
            ("is_a", "autoloads"),
            ("method_exists", "autoloads"),
            ("defined", "a class constant autoloads"),
            ("mb_strlen", "ValueError: unknown encoding"),
            ("dirname", "ValueError: levels < 1"),
            ("max", "ValueError: no elements"),
            ("json_encode", "JsonException under the flag"),
            ("preg_match_all", "ValueError: bad flags"),
            ("array_column", "TypeError: an array or object row value is no key"),
            ("gc_collect_cycles", "runs destructors"),
            ("stream_wrapper_register", "attaches user code"),
            ("stream_filter_register", "attaches user code"),
            ("in_array", "Error: two distinct recursive arrays compare"),
            ("array_search", "Error: two distinct recursive arrays compare"),
            ("array_keys", "Error: a recursive array against a search value"),
            ("array_unique", "Error: SORT_REGULAR compares recursive arrays"),
            ("sort", "Error: SORT_REGULAR compares recursive arrays"),
            ("rsort", "Error: SORT_REGULAR compares recursive arrays"),
            ("asort", "Error: SORT_REGULAR compares recursive arrays"),
            ("arsort", "Error: SORT_REGULAR compares recursive arrays"),
            ("array_replace_recursive", "Error: Recursion detected"),
            ("array_walk_recursive", "Error: Recursion detected"),
            ("date_create", "Error: an uninitialised DateTimeZone subclass"),
            ("date_create_immutable", "Error: an uninitialised DateTimeZone subclass"),
        ];
        for (name, why) in refused {
            assert!(!THROWLESS_NAMES.contains(&name), "{name}: {why}");
        }
    }
}
