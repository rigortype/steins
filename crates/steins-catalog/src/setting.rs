//! The ambient-setting cells, and the ini names that reach them (ADR-0101 §2.3, §3.11, issue
//! #1000, S6-core).
//!
//! A [`SettingCell`] is one process-owned cell of state a builtin reads implicitly and only the
//! script's own calls rewrite. Each cell has the label pair `global.read.setting.<cell>` and
//! `global.write.setting.<cell>`, which hang under the coarse `global.read` and `global.write`
//! labels by prefix, so every consumer that admits the parent admits the cell. A cell's labels
//! enter the registry ([`crate::known_labels`]) in the slice that colours its first row, and not
//! before. The environment cell's pair entered with `getenv` and `putenv` (ADR-0101 S6c), the last
//! cell to be registered.
//!
//! The first rows to name a cell other than the locale are the `ini_*` functions with a **literal
//! option name** ([`narrowed_ini_labels`]): `ini_set('precision', …)` reads and writes the
//! precision cell (it returns the entry's old value) and `ini_get('date.timezone')` reads the
//! timezone cell, where an argument-blind `ini_set` is the coarse `global.write`. A name this
//! module does not map, and a name the call does not spell, keep the coarse row.

/// One cell of ambient state: the roster of ADR-0101 §2.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingCell {
    /// `LC_*` as `setlocale` leaves it.
    Locale,
    /// `precision` and `serialize_precision`: one cell, because a call that reads one has no
    /// reason to be told apart from a call that reads the other, and `ini_set` of either is its
    /// write.
    Precision,
    /// The default timezone: `date_default_timezone_set` and the `date.timezone` ini.
    Timezone,
    /// The process environment block: `putenv` writes it, `getenv` reads it.
    Env,
    /// The default character set the `mb_*`, `iconv_*` and HTML functions fall back to, and the
    /// mbstring state they read without being handed it: the mb-regex encoding, options and
    /// syntax, the substitution character, the detect order, the language and the HTTP output
    /// encoding and the count of illegal characters the converters have met (ADR-0101 §3.13).
    Encoding,
    /// The residue: an ini value no other cell owns, one name at a time (`bcmath.scale`,
    /// `include_path`, `error_reporting`).
    Ini,
}

/// Indexed by `SettingCell as usize`; the same order as the enum.
static READ_LABELS: [&str; 6] = [
    "global.read.setting.locale",
    "global.read.setting.precision",
    "global.read.setting.timezone",
    "global.read.setting.env",
    "global.read.setting.encoding",
    "global.read.setting.ini",
];

/// Indexed by `SettingCell as usize`.
static WRITE_LABELS: [&str; 6] = [
    "global.write.setting.locale",
    "global.write.setting.precision",
    "global.write.setting.timezone",
    "global.write.setting.env",
    "global.write.setting.encoding",
    "global.write.setting.ini",
];

impl SettingCell {
    /// Every cell, in the registry's order of introduction.
    pub const ALL: [Self; 6] =
        [Self::Locale, Self::Precision, Self::Timezone, Self::Env, Self::Encoding, Self::Ini];

    /// The cell's name, the last segment of its labels.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Locale => "locale",
            Self::Precision => "precision",
            Self::Timezone => "timezone",
            Self::Env => "env",
            Self::Encoding => "encoding",
            Self::Ini => "ini",
        }
    }

    /// The label of a read of this cell, `global.read.setting.<cell>`.
    #[must_use]
    pub fn read_label(self) -> &'static str {
        READ_LABELS[self as usize]
    }

    /// The label of a write of this cell, `global.write.setting.<cell>`.
    #[must_use]
    pub fn write_label(self) -> &'static str {
        WRITE_LABELS[self as usize]
    }
}

/// The ini names each cell owns, where php-src (`php-8.5`) shows the entry feeding a reader or
/// a writer of the cell. A name is spelled exactly: the engine finds an ini entry by a
/// case-sensitive hash lookup (`zend_ini_get_value`, `zend_alter_ini_entry_ex`), so `PRECISION`
/// names no entry and `ini_get('PRECISION')` is `false`.
///
/// | cell | names | what reads them |
/// | --- | --- | --- |
/// | precision | `precision`, `serialize_precision` | `EG(precision)` (`smart_str_append_double`, the float-to-string conversions) and `PG(serialize_precision)` (`var_export`, `json_encode`, `serialize`, `var_dump`) |
/// | timezone | `date.timezone` | `guess_timezone`, once no `date_default_timezone_set` has run |
/// | encoding | `default_charset`, `internal_encoding`, `input_encoding`, `output_encoding`, `mbstring.internal_encoding`, `iconv.{internal,input,output}_encoding`, `mbstring.{language,detect_order,http_input,http_output,substitute_character,strict_detection}` | the defaults of the `mb_*`, `iconv_*` and HTML functions that take an `$encoding`, of `mb_ereg*` and `mb_split`, and of `mb_detect_encoding`, `mb_language`, `mb_http_input`, `mb_http_output` and `mb_substitute_character` |
/// | ini | `bcmath.scale`, `include_path`, `error_reporting` | `bc*` without a scale, `get_include_path`, `error_reporting()` |
///
/// Five more entries feed the encoding readers and also reset the mb-regex encoding:
/// `default_charset`, `internal_encoding`, `input_encoding`, `output_encoding` and
/// `mbstring.internal_encoding`. Rewriting any of them runs
/// `_php_mb_ini_mbstring_internal_encoding_set` (`mbstring.c`), which calls
/// `php_mb_regex_set_default_mbctype`, the state `mb_regex_encoding()` writes. S6-core left them
/// on the coarse row because that state belonged to no cell; S6d puts the mb-regex state into the
/// encoding cell (ADR-0101 §3.13), so a write of any of the five is the cell's write.
///
/// `mbstring.encoding_translation` and `mbstring.http_output_conv_mimetypes` feed an output
/// handler and have no reader a row names: they map to no cell and keep the coarse row.
/// `mbstring.regex_stack_limit` and `mbstring.regex_retry_limit` are read by every `mb_ereg*`
/// search (`_php_mb_onig_search`) and change a result silently
/// (`mb_ereg('(a+)+c|x', str_repeat('a', 18) . 'bx')` is `true`, and `false` after
/// `ini_set('mbstring.regex_retry_limit', '1000')`), so they map to the cell, which holds the
/// mb-regex state. The locale and environment cells own no ini.
const INI_NAMES: &[(&str, SettingCell)] = &[
    ("precision", SettingCell::Precision),
    ("serialize_precision", SettingCell::Precision),
    ("date.timezone", SettingCell::Timezone),
    ("mbstring.regex_retry_limit", SettingCell::Encoding),
    ("mbstring.regex_stack_limit", SettingCell::Encoding),
    ("default_charset", SettingCell::Encoding),
    ("internal_encoding", SettingCell::Encoding),
    ("input_encoding", SettingCell::Encoding),
    ("output_encoding", SettingCell::Encoding),
    ("mbstring.internal_encoding", SettingCell::Encoding),
    ("iconv.internal_encoding", SettingCell::Encoding),
    ("iconv.input_encoding", SettingCell::Encoding),
    ("iconv.output_encoding", SettingCell::Encoding),
    ("mbstring.language", SettingCell::Encoding),
    ("mbstring.detect_order", SettingCell::Encoding),
    ("mbstring.http_input", SettingCell::Encoding),
    ("mbstring.http_output", SettingCell::Encoding),
    ("mbstring.substitute_character", SettingCell::Encoding),
    ("mbstring.strict_detection", SettingCell::Encoding),
    ("bcmath.scale", SettingCell::Ini),
    ("include_path", SettingCell::Ini),
    ("error_reporting", SettingCell::Ini),
];

/// The cell the ini entry `name` feeds, or `None` for a name no cell owns.
#[must_use]
pub fn ini_cell(name: &str) -> Option<SettingCell> {
    INI_NAMES.iter().find(|(ini, _)| *ini == name).map(|&(_, cell)| cell)
}

/// How an ini function reaches the entry it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IniAccess {
    /// `ini_get`: reads the entry.
    Get,
    /// `ini_set` and `ini_alter` (an alias of `ini_set`): return the entry's old value
    /// (`zend_ini_get_value`, unconditionally) and rewrite it, so a read and a write; the new
    /// value is rendered as a string first, which reads `precision` where it is a float.
    Set,
    /// `ini_restore`: rewrites the entry and returns nothing.
    Restore,
}

/// An ini function call whose option name is a literal that a cell owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IniCall {
    /// The cell that owns the option.
    pub cell: SettingCell,
    /// What the function does to it.
    pub access: IniAccess,
}

impl IniCall {
    /// The labels of the call: the cell's read for `ini_get`, its write for `ini_restore`, and
    /// both for `ini_set`, which hands back the old value.
    #[must_use]
    pub fn labels(self) -> &'static [&'static str] {
        let i = self.cell as usize;
        match self.access {
            IniAccess::Get => std::slice::from_ref(&READ_LABELS[i]),
            IniAccess::Restore => std::slice::from_ref(&WRITE_LABELS[i]),
            IniAccess::Set => &SET_LABELS[i],
        }
    }
}

/// `[read, write]` per cell, indexed by `SettingCell as usize`.
static SET_LABELS: [[&str; 2]; 6] = [
    [READ_LABELS[0], WRITE_LABELS[0]],
    [READ_LABELS[1], WRITE_LABELS[1]],
    [READ_LABELS[2], WRITE_LABELS[2]],
    [READ_LABELS[3], WRITE_LABELS[3]],
    [READ_LABELS[4], WRITE_LABELS[4]],
    [READ_LABELS[5], WRITE_LABELS[5]],
];

/// The ini function call `name(option, …)` of `positional` arguments, when `option` is a written
/// literal that [`ini_cell`] maps and the count is the function's own (one, two, two, one for
/// `ini_get`, `ini_set`, `ini_alter`, `ini_restore`): another count raises an
/// `ArgumentCountError` before it touches an entry, and a named or spread list has no count to
/// read, so both are `None`.
#[must_use]
pub fn ini_call(name: &str, option: &str, positional: usize) -> Option<IniCall> {
    // The option leads: nearly every builtin call that spells a first literal names no ini entry,
    // and answers before paying for a lowercase copy of the function's name.
    let cell = ini_cell(option)?;
    let access = match name.to_ascii_lowercase().as_str() {
        "ini_get" if positional == 1 => IniAccess::Get,
        "ini_set" | "ini_alter" if positional == 2 => IniAccess::Set,
        "ini_restore" if positional == 1 => IniAccess::Restore,
        _ => return None,
    };
    Some(IniCall { cell, access })
}

/// The **narrowed** labels of an `ini_get`, `ini_set`, `ini_alter` or `ini_restore` call whose
/// option name is a written literal that [`ini_cell`] maps ([`ini_call`]), or `None`: the caller
/// keeps [`effect_labels`](crate::effect_labels)' coarse row, `global.read` for `ini_get` and
/// `global.write` for the others.
///
/// `ini_get` reads the entry on every call; `ini_set` and `ini_alter` return the old value, which
/// is a read of the entry whatever happens to the write, and rewrite it, so they carry the cell's
/// read and write (php-src's `zif_ini_set` calls `zend_ini_get_value` first, unconditionally);
/// `ini_restore` rewrites it and returns nothing. The cell's labels are not the whole of the
/// setting effect of an `ini_set`: its value argument is rendered as a string, which reads
/// `precision` where it is a float, and the effects pass decides that read from the argument
/// ([`IniAccess::Set`]).
#[must_use]
pub fn narrowed_ini_labels(
    name: &str,
    option: &str,
    positional: usize,
) -> Option<&'static [&'static str]> {
    ini_call(name, option, positional).map(IniCall::labels)
}

#[cfg(test)]
mod tests {
    use super::{INI_NAMES, SettingCell, ini_cell, narrowed_ini_labels};
    use crate::{is_known_label, subsumes};

    /// The labels spell the cell's name, and the two statics follow the enum's order.
    #[test]
    fn a_cell_names_its_label_pair() {
        for cell in SettingCell::ALL {
            assert_eq!(cell.read_label(), format!("global.read.setting.{}", cell.name()));
            assert_eq!(cell.write_label(), format!("global.write.setting.{}", cell.name()));
            assert!(subsumes("global.read.setting", cell.read_label()), "{cell:?}");
            assert!(subsumes("global.write.setting", cell.write_label()), "{cell:?}");
            assert!(!subsumes("global.read", cell.write_label()), "{cell:?}: a write is no read");
        }
        assert_eq!(SettingCell::ALL.len(), 6);
    }

    /// A cell's labels are registered with its first coloured row (ADR-0101 §2.2): every cell has
    /// one now, the environment's since `getenv` and `putenv` (S6c).
    #[test]
    fn a_cell_is_registered_once_a_row_colours_it() {
        for cell in SettingCell::ALL {
            assert!(is_known_label(cell.read_label()), "{cell:?}");
            assert!(is_known_label(cell.write_label()), "{cell:?}");
        }
    }

    /// Every name the table owns is spelled once, and no name maps to a cell that is not
    /// registered.
    #[test]
    fn the_ini_table_is_a_function_into_registered_cells() {
        for (i, (name, cell)) in INI_NAMES.iter().enumerate() {
            assert!(INI_NAMES[i + 1..].iter().all(|(other, _)| other != name), "{name} twice");
            assert!(is_known_label(cell.read_label()) && is_known_label(cell.write_label()), "{name}");
            assert_eq!(ini_cell(name), Some(*cell), "{name}");
        }
        assert!(INI_NAMES.iter().all(|(_, cell)| !matches!(cell, SettingCell::Locale | SettingCell::Env)));
    }

    /// One name per cell that owns any, the two precision inis sharing a cell, the residue naming
    /// its ini one at a time, and the names php-src gives no reader a row names yet unmapped.
    #[test]
    fn the_ini_table_maps_the_names_php_src_feeds_a_cell() {
        use SettingCell::{Encoding, Ini, Precision, Timezone};
        for (name, cell) in [
            ("precision", Precision),
            ("serialize_precision", Precision),
            ("date.timezone", Timezone),
            ("iconv.internal_encoding", Encoding),
            ("mbstring.language", Encoding),
            // These five also reset the mb-regex encoding, which the cell holds since S6d.
            ("default_charset", Encoding),
            ("internal_encoding", Encoding),
            ("input_encoding", Encoding),
            ("output_encoding", Encoding),
            ("mbstring.internal_encoding", Encoding),
            ("mbstring.regex_retry_limit", Encoding),
            ("mbstring.regex_stack_limit", Encoding),
            ("bcmath.scale", Ini),
            ("include_path", Ini),
            ("error_reporting", Ini),
        ] {
            assert_eq!(ini_cell(name), Some(cell), "{name}");
        }
        for name in [
            "", "display_errors", "memory_limit", "max_execution_time", "pcre.backtrack_limit",
            "mbstring.encoding_translation", "mbstring.foo",
            "date.default_latitude", "intl.default_locale", "setlocale", "locale",
            // The engine finds an entry by an exact, case-sensitive lookup.
            "PRECISION", "Precision", "Date.Timezone", "precision ", "precision\0",
        ] {
            assert_eq!(ini_cell(name), None, "{name:?}");
        }
    }

    /// The function decides read, write or both, its arity decides whether the call is read at all, and
    /// the option name decides the cell.
    #[test]
    fn a_literal_name_narrows_to_its_cells_read_or_write() {
        let read = |cell: SettingCell| Some(vec![cell.read_label()]);
        let write = |cell: SettingCell| Some(vec![cell.write_label()]);
        // `ini_set` returns the old value, which is a read of the entry.
        let both = |cell: SettingCell| Some(vec![cell.read_label(), cell.write_label()]);
        let narrowed = |name, option, n| narrowed_ini_labels(name, option, n).map(<[_]>::to_vec);
        assert_eq!(narrowed("ini_get", "precision", 1), read(SettingCell::Precision));
        assert_eq!(narrowed("ini_set", "precision", 2), both(SettingCell::Precision));
        assert_eq!(narrowed("ini_alter", "serialize_precision", 2), both(SettingCell::Precision));
        assert_eq!(narrowed("ini_restore", "date.timezone", 1), write(SettingCell::Timezone));
        assert_eq!(narrowed("INI_SET", "mbstring.language", 2), both(SettingCell::Encoding));
        assert_eq!(narrowed("ini_get", "include_path", 1), read(SettingCell::Ini));
        // The wrong arity raises before an entry is touched; a named or spread list has none.
        for (name, n) in [("ini_get", 0), ("ini_get", 2), ("ini_set", 1), ("ini_set", 3),
            ("ini_alter", 1), ("ini_restore", 0), ("ini_restore", 2)]
        {
            assert_eq!(narrowed(name, "precision", n), None, "{name}/{n}");
        }
        // An unmapped name keeps the coarse row, and so does a name that is no ini function.
        assert_eq!(narrowed("ini_set", "display_errors", 2), None);
        assert_eq!(narrowed("ini_set", "default_charset", 2), both(SettingCell::Encoding));
        assert_eq!(narrowed("ini_get", "mbstring.internal_encoding", 1), read(SettingCell::Encoding));
        assert_eq!(narrowed("ini_get_all", "precision", 1), None);
        assert_eq!(narrowed("putenv", "precision", 1), None);
    }

    /// `ini_set` and `ini_alter` hand back the old value, so they read the cell they write;
    /// `ini_restore` returns nothing and only writes; `ini_get` only reads.
    #[test]
    fn ini_set_reads_the_old_value_and_restore_does_not() {
        use super::{IniAccess, ini_call};
        for (name, access) in [
            ("ini_get", IniAccess::Get),
            ("ini_set", IniAccess::Set),
            ("ini_alter", IniAccess::Set),
            ("ini_restore", IniAccess::Restore),
        ] {
            let n = if access == IniAccess::Set { 2 } else { 1 };
            let call = ini_call(name, "bcmath.scale", n).expect(name);
            assert_eq!(call.cell, SettingCell::Ini);
            assert_eq!(call.access, access, "{name}");
            let cell = call.cell;
            let expected: Vec<&str> = match access {
                IniAccess::Get => vec![cell.read_label()],
                IniAccess::Set => vec![cell.read_label(), cell.write_label()],
                IniAccess::Restore => vec![cell.write_label()],
            };
            assert_eq!(call.labels(), expected, "{name}");
        }
    }
}
