//! The locale readers whose read a **mode argument** decides (ADR-0101 §3.9, issue #1000, S4).
//!
//! Most readers of the locale cell read it on every call to them: `ctype_alpha`, `basename`,
//! `strnatcmp` consult it whatever they were given, so [`effect_labels`](crate::effect_labels)
//! states the read and a call site has nothing to decide. Three names read it only under a mode
//! the caller selects with an argument, like the printf family's format:
//!
//! | name | argument | reads the locale when |
//! | --- | --- | --- |
//! | `sort`, `rsort`, `asort`, `arsort` | `$flags` (position 1) | its base type is `SORT_LOCALE_STRING` (`strcoll`) or `SORT_NATURAL` (`strnatcmp`'s `isspace`, `isdigit`, `toupper`) |
//! | `ksort`, `krsort` | `$flags` (position 1) | the same, or the base type is `SORT_STRING` with `SORT_FLAG_CASE`: the key comparison folds case through `tolower` where the data sorts use the engine's ASCII table |
//! | `substr_compare` | `$case_insensitive` (position 4) | it is true (`zend_binary_strncasecmp_l`) |
//! | `pathinfo` | `$flags` (position 1) | it asks for the basename, extension or filename (`php_basename`); the directory name alone reads nothing |
//!
//! A row is the upper bound and the call site decides, in the three ways the criterion of
//! ADR-0101 §3.2 allows: a literal argument proves the read or drops it, an omitted one is the
//! parameter's default, and any other (a variable, an expression the scan cannot evaluate, a
//! named or spread argument list) is the `value-dependent-read` gap and never a label. The
//! catalog states the decision ([`LocaleReadGate::reads`]); the effects pass reads the call.
//!
//! The rows follow `PINNED_PHP` (8.5), as every row does: on 8.1 the data sorts under
//! `SORT_STRING | SORT_FLAG_CASE` also fold case through `tolower` (`string_case_compare_function`
//! became ASCII-only in 8.2).

/// What a call shows of the argument that decides a gated read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateArg {
    /// The call does not supply the argument: the parameter's default decides.
    Omitted,
    /// An integer the scan evaluated (a literal, an engine constant, a `|` of such terms).
    Int(i64),
    /// A literal `true` or `false`.
    Bool(bool),
}

/// Which argument of a gated reader decides its read, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocaleReadGate {
    position: usize,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A sort `$flags` of a data sort.
    DataSortFlags,
    /// A sort `$flags` of a key sort.
    KeySortFlags,
    /// `substr_compare`'s `$case_insensitive`.
    CaseInsensitive,
    /// `pathinfo`'s `$flags`.
    PathinfoFlags,
}

/// `PHP_SORT_FLAG_CASE`, `PHP_SORT_STRING`, `PHP_SORT_LOCALE_STRING`, `PHP_SORT_NATURAL`.
const SORT_FLAG_CASE: i64 = 8;
const SORT_STRING: i64 = 2;
const SORT_LOCALE_STRING: i64 = 5;
const SORT_NATURAL: i64 = 6;
/// The three `pathinfo` parts `php_basename` produces: `PATHINFO_BASENAME`, `PATHINFO_EXTENSION`
/// and `PATHINFO_FILENAME`. `PATHINFO_DIRNAME` (1) is `zend_dirname`'s and reads nothing.
const PATHINFO_BASENAME_PARTS: i64 = 2 | 4 | 8;

/// The gate of the builtin `name` (case-insensitive), or `None` for a name whose read is
/// unconditional or absent.
#[must_use]
pub fn locale_read_gate(name: &str) -> Option<LocaleReadGate> {
    let (position, kind) = match name.to_ascii_lowercase().as_str() {
        "sort" | "rsort" | "asort" | "arsort" => (1, Kind::DataSortFlags),
        "ksort" | "krsort" => (1, Kind::KeySortFlags),
        "substr_compare" => (4, Kind::CaseInsensitive),
        "pathinfo" => (1, Kind::PathinfoFlags),
        _ => return None,
    };
    Some(LocaleReadGate { position, kind })
}

impl LocaleReadGate {
    /// The positional index of the deciding argument.
    #[must_use]
    pub const fn position(self) -> usize {
        self.position
    }

    /// Whether the read happens at a call whose deciding argument shows `arg`, or `None` when
    /// the argument is of a shape this gate cannot decide on (an integer where the gate wants
    /// a boolean): the caller treats that as the call showing nothing.
    #[must_use]
    pub fn reads(self, arg: GateArg) -> Option<bool> {
        match (self.kind, arg) {
            // `$flags = SORT_REGULAR`, `$case_insensitive = false`, `$flags = PATHINFO_ALL`.
            (Kind::PathinfoFlags, GateArg::Omitted) => Some(true),
            (_, GateArg::Omitted) => Some(false),
            (Kind::DataSortFlags, GateArg::Int(flags)) => Some(reads_by_sort_type(flags)),
            (Kind::KeySortFlags, GateArg::Int(flags)) => {
                Some(reads_by_sort_type(flags) || folds_key_case(flags))
            }
            (Kind::CaseInsensitive, GateArg::Bool(flag)) => Some(flag),
            (Kind::PathinfoFlags, GateArg::Int(flags)) => {
                Some(flags & PATHINFO_BASENAME_PARTS != 0)
            }
            _ => None,
        }
    }
}

/// `php_get_data_compare_func`'s and `php_get_key_compare_func`'s dispatch: the type is the flags
/// with `SORT_FLAG_CASE` removed, and a type that is not one of the named ones is `SORT_REGULAR`.
fn reads_by_sort_type(flags: i64) -> bool {
    matches!(flags & !SORT_FLAG_CASE, SORT_LOCALE_STRING | SORT_NATURAL)
}

/// `php_array_key_compare_string_case_unstable_i`: a key sort of strings folded to one case.
fn folds_key_case(flags: i64) -> bool {
    flags & SORT_FLAG_CASE != 0 && flags & !SORT_FLAG_CASE == SORT_STRING
}

#[cfg(test)]
mod tests {
    use super::{GateArg, locale_read_gate};

    fn reads(name: &str, arg: GateArg) -> Option<bool> {
        locale_read_gate(name).expect(name).reads(arg)
    }

    /// The gated names, and the positions their deciding arguments take.
    #[test]
    fn the_gated_names_and_positions() {
        for name in ["sort", "rsort", "asort", "arsort", "ksort", "krsort", "PathInfo"] {
            assert_eq!(locale_read_gate(name).expect(name).position(), 1, "{name}");
        }
        assert_eq!(locale_read_gate("substr_compare").expect("row").position(), 4);
        for name in ["usort", "natsort", "array_multisort", "strnatcmp", "basename", "strcmp"] {
            assert_eq!(locale_read_gate(name), None, "{name}");
        }
    }

    /// A data sort reads under `SORT_LOCALE_STRING` and `SORT_NATURAL` with or without
    /// `SORT_FLAG_CASE`, and under nothing else; a key sort also under the case-folding string
    /// sort.
    #[test]
    fn the_sort_flags_that_read() {
        for flags in [5, 6, 5 | 8, 6 | 8, 13, 14] {
            assert_eq!(reads("sort", GateArg::Int(flags)), Some(true), "{flags}");
            assert_eq!(reads("krsort", GateArg::Int(flags)), Some(true), "{flags}");
        }
        for flags in [0, 1, 2, 3, 4, 8, 9, 10, 7, 15, 16, -1] {
            assert_eq!(reads("asort", GateArg::Int(flags)), Some(false), "{flags}");
        }
        assert_eq!(reads("ksort", GateArg::Int(2 | 8)), Some(true), "keys fold case by tolower");
        assert_eq!(reads("ksort", GateArg::Int(2)), Some(false));
        assert_eq!(reads("ksort", GateArg::Int(1 | 8)), Some(false));
        assert_eq!(reads("rsort", GateArg::Omitted), Some(false), "SORT_REGULAR");
        assert_eq!(reads("sort", GateArg::Bool(true)), None);
    }

    /// `substr_compare` reads only when case-insensitive, `pathinfo` unless only the
    /// directory is asked for.
    #[test]
    fn the_other_gates() {
        assert_eq!(reads("substr_compare", GateArg::Omitted), Some(false));
        assert_eq!(reads("substr_compare", GateArg::Bool(false)), Some(false));
        assert_eq!(reads("substr_compare", GateArg::Bool(true)), Some(true));
        assert_eq!(reads("substr_compare", GateArg::Int(1)), None);
        assert_eq!(reads("pathinfo", GateArg::Omitted), Some(true), "PATHINFO_ALL");
        for flags in [15, 2, 4, 8, 1 | 4, 3, 14] {
            assert_eq!(reads("pathinfo", GateArg::Int(flags)), Some(true), "{flags}");
        }
        for flags in [1, 0, 16] {
            assert_eq!(reads("pathinfo", GateArg::Int(flags)), Some(false), "{flags}");
        }
    }

    /// Every gated name carries the read on its row, as the upper bound the gate narrows.
    #[test]
    fn a_gated_name_carries_the_read_on_its_row() {
        for name in
            ["sort", "rsort", "asort", "arsort", "ksort", "krsort", "substr_compare", "pathinfo"]
        {
            let row = crate::effect_labels(name);
            assert!(
                row.is_some_and(|l| l.contains(&"global.read.setting.locale")),
                "{name}"
            );
        }
    }
}
