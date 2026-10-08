//! The encoding cell's gated readers (ADR-0101 §3.13, issue #1000, S6d).
//!
//! The encoding cell is the default character set the `mb_*`, `iconv_*` and HTML functions fall
//! back to: `default_charset` and the `internal_encoding` family of ini entries, what
//! `mb_internal_encoding()` and `mb_regex_encoding()` set, and the other mbstring state a call
//! reads without being handed it (the substitution character, the detect order, the language,
//! the regex options). A function that takes an `$encoding` reads the cell where the argument is
//! omitted or `null`; the HTML functions also where it is the empty string
//! (`determine_charset`, `ext/standard/html.c`). php-src (`php-8.5.11`) decides the rest:
//!
//! | class | functions | literal encoding | notes |
//! | --- | --- | --- | --- |
//! | plain | `mb_strlen mb_strwidth mb_strpos mb_strrpos mb_stripos mb_strripos mb_substr_count mb_strcut mb_check_encoding mb_chr mb_ord` | no read | nothing else of the cell is consulted: the conversions they run take constant error modes (`MBFL_OUTPUTFILTER_ILLEGAL_MODE_BADUTF8`) |
//! | substituting | the other 21 `mb_*` that take `$encoding` (`mb_substr`, `mb_strtoupper`, `mb_convert_encoding`, …) | **undecided** | they rebuild the string through `mb_convert_buf_init` or `php_unicode_convert_case` with `MBSTRG(current_filter_illegal_substchar)` and `MBSTRG(current_filter_illegal_mode)`, which `mb_substitute_character()` and `mbstring.substitute_character` write: an input with an invalid sequence, or a character the target cannot hold, reads the cell whatever encoding is named. Whether the subject holds one is a value the call does not show |
//! | HTML | `htmlspecialchars htmlentities` | no read | both return on an empty string before `determine_charset` |
//! | HTML | `html_entity_decode` | no read | returns the string on one with no `&` before `determine_charset` |
//! | HTML | `get_html_translation_table` | no read | |
//! | iconv | `iconv_strlen iconv_substr iconv_strpos iconv_mime_decode iconv_mime_decode_headers` | no read, but `''`, `char` and `locale` are undecided | a `null` charset takes `get_internal_encoding()`; the C library takes the locale's own charset for an empty name |
//! | iconv | `iconv_strrpos` | the same | and an empty needle returns `false` before the charset is looked at |
//!
//! The omitted or `null` argument is the proven read except where the class lists an early
//! return: there the subject decides, and a subject the call does not show is the
//! `value-dependent-read` gap.
//!
//! Four more shapes read or write the cell by their arity, not by an encoding argument:
//!
//! * **accessors** (`mb_internal_encoding`, `mb_regex_encoding`, `mb_http_output`,
//!   `mb_detect_order`, `mb_language`, `mb_substitute_character`): omitted or `null` returns the
//!   current value (a read); any other argument sets it and returns `true` (a write);
//! * `mb_regex_set_options` returns the **previous** options whatever it is given (a read) and
//!   sets them when given a string (a write);
//! * `iconv_get_encoding` reads the three iconv entries for `all` and each of their names, and
//!   returns `false` for any other;
//! * `iconv_set_encoding`, `mb_ereg`, `mb_eregi`, `mb_ereg_replace`, `mb_eregi_replace`,
//!   `mb_ereg_match` and `mb_split` carry their rows with no gate: the regex functions compile
//!   under `MBREX(current_mbctype)` and the default options, and `iconv_set_encoding` writes an
//!   ini entry.
//!
//! The 42 gated `$encoding` readers are the functions whose generated parameter list
//! (`param_facts_generated.rs`) names a parameter `encoding` or `from_encoding`, less the ones
//! listed in `EXCLUDED` (with the test that holds the partition) with the reason each is not the
//! cell's.

use super::GateArg;
use crate::param_facts;

/// How a function reads the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Gate {
    /// The function takes an `$encoding` at `position`.
    Argument { position: u8, class: Class },
    /// `mb_internal_encoding` and the other accessors: read with no argument, write with one.
    Accessor,
    /// `mb_regex_set_options`: reads on every call, writes when given a string.
    RegexOptions,
    /// `iconv_get_encoding`: reads for the `$type` it knows.
    IconvType,
}

/// The classes of the table in the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Plain,
    Substituting,
    HtmlEncode,
    HtmlTable,
    HtmlDecode,
    Iconv,
    IconvNeedle,
}

const PLAIN: &[&str] = &[
    "mb_check_encoding",
    "mb_chr",
    "mb_ord",
    "mb_strcut",
    "mb_stripos",
    "mb_strlen",
    "mb_strpos",
    "mb_strripos",
    "mb_strrpos",
    "mb_strwidth",
    "mb_substr_count",
];

const SUBSTITUTING: &[&str] = &[
    "mb_convert_case",
    "mb_convert_encoding",
    "mb_convert_kana",
    "mb_decode_numericentity",
    "mb_encode_numericentity",
    "mb_lcfirst",
    "mb_ltrim",
    "mb_rtrim",
    "mb_scrub",
    "mb_str_pad",
    "mb_str_split",
    "mb_strimwidth",
    "mb_stristr",
    "mb_strrchr",
    "mb_strrichr",
    "mb_strstr",
    "mb_strtolower",
    "mb_strtoupper",
    "mb_substr",
    "mb_trim",
    "mb_ucfirst",
];

const ICONV: &[&str] = &[
    "iconv_mime_decode",
    "iconv_mime_decode_headers",
    "iconv_strlen",
    "iconv_strpos",
    "iconv_substr",
];

/// The accessors: the name takes one optional parameter that is the new value.
pub(super) const ACCESSORS: &[&str] = &[
    "mb_detect_order",
    "mb_http_output",
    "mb_internal_encoding",
    "mb_language",
    "mb_regex_encoding",
    "mb_substitute_character",
];

/// The functions whose generated parameter list names an `encoding` or `from_encoding` and that
/// are **not** gated `$encoding` readers, with why. The accessors are in [`ACCESSORS`]. The
/// partition is held by a test, so the table exists there only.
#[cfg(test)]
pub(super) const EXCLUDED: &[(&str, &str)] = &[
    ("deflate_init", "a ZLIB_ENCODING_* constant"),
    ("inflate_init", "a ZLIB_ENCODING_* constant"),
    ("gzcompress", "a ZLIB_ENCODING_* constant"),
    ("gzdeflate", "a ZLIB_ENCODING_* constant"),
    ("gzencode", "a ZLIB_ENCODING_* constant"),
    ("zlib_encode", "a ZLIB_ENCODING_* constant"),
    ("openssl_cms_decrypt", "a CMS serialisation constant"),
    ("openssl_cms_encrypt", "a CMS serialisation constant"),
    ("openssl_cms_sign", "a CMS serialisation constant"),
    ("openssl_cms_verify", "a CMS serialisation constant"),
    ("pg_set_client_encoding", "a required connection argument, no default"),
    ("pg_setclientencoding", "a required connection argument, no default"),
    ("tidy_parse_file", "the document's own encoding, `utf8` unless named"),
    ("tidy_parse_string", "the document's own encoding, `utf8` unless named"),
    ("tidy_repair_file", "the document's own encoding, `utf8` unless named"),
    ("tidy_repair_string", "the document's own encoding, `utf8` unless named"),
    ("xml_parser_create", "the parser's own default, `UTF-8`"),
    ("xml_parser_create_ns", "the parser's own default, `UTF-8`"),
    ("xmlwriter_start_document", "`null` writes no declaration"),
    ("iconv", "both charsets are required"),
    ("iconv_set_encoding", "a write row, no encoding argument to omit"),
    ("mb_convert_variables", "both encodings are required"),
    ("mb_encoding_aliases", "the encoding is required"),
    ("mb_preferred_mime_name", "the encoding is required"),
];

/// The gate of the lowercase builtin `name`, or `None`.
pub(super) fn gate_of(name: &str) -> Option<Gate> {
    if ACCESSORS.contains(&name) {
        return Some(Gate::Accessor);
    }
    let class = match name {
        "mb_regex_set_options" => return Some(Gate::RegexOptions),
        "iconv_get_encoding" => return Some(Gate::IconvType),
        "htmlspecialchars" | "htmlentities" => Class::HtmlEncode,
        "get_html_translation_table" => Class::HtmlTable,
        "html_entity_decode" => Class::HtmlDecode,
        "iconv_strrpos" => Class::IconvNeedle,
        _ if PLAIN.contains(&name) => Class::Plain,
        _ if SUBSTITUTING.contains(&name) => Class::Substituting,
        _ if ICONV.contains(&name) => Class::Iconv,
        _ => return None,
    };
    Some(Gate::Argument { position: encoding_position(name)?, class })
}

/// The position of the `$encoding` (or `$from_encoding`) parameter in the generated table.
fn encoding_position(name: &str) -> Option<u8> {
    let names = param_facts(name)?.param_names;
    let at = names.iter().position(|n| matches!(*n, "encoding" | "from_encoding"))?;
    u8::try_from(at).ok()
}

impl Class {
    /// Whether the class has a subject that returns before the encoding is looked at.
    const fn guarded(self) -> bool {
        matches!(self, Self::HtmlEncode | Self::HtmlDecode | Self::IconvNeedle)
    }

    /// The read when the encoding argument names the default: omitted, `null`, or (for HTML)
    /// the empty string.
    fn default_read(self, guard: Option<GateArg<'_>>) -> Option<bool> {
        let text = || match guard {
            Some(GateArg::Str(text)) => Some(text),
            _ => None,
        };
        match self {
            Self::Plain | Self::Substituting | Self::HtmlTable | Self::Iconv => Some(true),
            Self::HtmlEncode | Self::IconvNeedle => text().map(|t| !t.is_empty()),
            Self::HtmlDecode => text().map(|t| t.contains('&')),
        }
    }

    /// The read when the encoding argument is the string literal `name`.
    fn named_read(self, name: &str, guard: Option<GateArg<'_>>) -> Option<bool> {
        match self {
            Self::Plain => Some(false),
            Self::Substituting => None,
            Self::HtmlEncode | Self::HtmlTable | Self::HtmlDecode if name.is_empty() => {
                self.default_read(guard)
            }
            Self::HtmlEncode | Self::HtmlTable | Self::HtmlDecode => Some(false),
            Self::Iconv | Self::IconvNeedle => {
                let locale_charset = name.is_empty()
                    || name.eq_ignore_ascii_case("char")
                    || name.eq_ignore_ascii_case("locale");
                (!locale_charset).then_some(false)
            }
        }
    }
}

impl Gate {
    /// The positional indexes of the deciding arguments: the guarding subject first, where the
    /// class has one.
    pub(super) const fn positions(self) -> &'static [usize] {
        match self {
            Self::Argument { class: Class::HtmlEncode | Class::HtmlDecode, .. } => &[0, 2],
            Self::Argument { class: Class::IconvNeedle, .. } => &[1, 2],
            Self::Argument { position, .. } => single(position),
            Self::Accessor | Self::RegexOptions | Self::IconvType => &[0],
        }
    }

    /// See [`SettingReadGate::reads`](super::SettingReadGate::reads).
    pub(super) fn reads(self, args: &[Option<GateArg<'_>>]) -> Option<bool> {
        match self {
            Self::Argument { class, .. } => {
                let (guard, encoding) = if class.guarded() {
                    (args.first().copied().flatten(), args.get(1).copied().flatten())
                } else {
                    (None, args.first().copied().flatten())
                };
                match encoding? {
                    GateArg::Omitted | GateArg::Null => class.default_read(guard),
                    GateArg::Str(name) => class.named_read(name, guard),
                    _ => None,
                }
            }
            Self::Accessor => match args.first().copied().flatten()? {
                GateArg::Omitted | GateArg::Null => Some(true),
                // Anything that is not `null` sets (a bad value throws before it does).
                GateArg::Str(_) | GateArg::Int(_) | GateArg::Bool(_) | GateArg::NotText => {
                    Some(false)
                }
                GateArg::Strs(_) => None,
            },
            Self::RegexOptions => Some(true),
            Self::IconvType => match args.first().copied().flatten()? {
                GateArg::Omitted => Some(true),
                GateArg::Str(kind) => Some(
                    ["all", "input_encoding", "output_encoding", "internal_encoding"]
                        .iter()
                        .any(|known| kind.eq_ignore_ascii_case(known)),
                ),
                _ => None,
            },
        }
    }

    /// Whether the call may write the cell: `false` only for an accessor or `mb_regex_set_options`
    /// shown to have no argument to set from.
    pub(super) fn writes(self, args: &[Option<GateArg<'_>>]) -> bool {
        match self {
            Self::Accessor | Self::RegexOptions => {
                !matches!(args.first().copied().flatten(), Some(GateArg::Omitted | GateArg::Null))
            }
            Self::Argument { .. } | Self::IconvType => true,
        }
    }
}

/// The one-element position slices for the positions an `$encoding` takes.
const fn single(position: u8) -> &'static [usize] {
    match position {
        0 => &[0],
        1 => &[1],
        2 => &[2],
        3 => &[3],
        4 => &[4],
        _ => &[5],
    }
}

#[cfg(test)]
mod tests {
    use super::{ACCESSORS, EXCLUDED, Gate, GateArg, ICONV, PLAIN, SUBSTITUTING, gate_of};
    use crate::{SettingCell, effect_labels, setting_read_gate};

    const READ: &str = "global.read.setting.encoding";
    const WRITE: &str = "global.write.setting.encoding";

    /// Every function whose generated parameter list names an `encoding` or `from_encoding` is
    /// gated, an accessor, or excluded with a reason; no name is two of those.
    #[test]
    fn every_encoding_parameter_is_gated_an_accessor_or_excluded() {
        let mut gated = 0;
        for (name, facts) in crate::param_facts_generated::PARAM_FACTS {
            if !facts.param_names.iter().any(|n| matches!(*n, "encoding" | "from_encoding")) {
                continue;
            }
            let excluded = EXCLUDED.iter().any(|(n, _)| n == name);
            let accessor = ACCESSORS.contains(name);
            let argument = matches!(gate_of(name), Some(Gate::Argument { .. }));
            assert_eq!(
                [excluded, accessor, argument].iter().filter(|x| **x).count(),
                1,
                "{name} must be exactly one of excluded, an accessor, a gated reader"
            );
            gated += usize::from(argument);
        }
        assert_eq!(gated, 42);
        assert_eq!(PLAIN.len() + SUBSTITUTING.len() + ICONV.len() + 5, 42);
        for (name, _) in EXCLUDED {
            assert!(gate_of(name).is_none(), "{name}");
        }
    }

    /// A gated reader's `$encoding` position is the generated table's, and the guarded classes
    /// list their subject first at the position the table gives.
    #[test]
    fn the_positions_follow_the_generated_table() {
        for name in PLAIN.iter().chain(SUBSTITUTING).chain(ICONV) {
            let Some(Gate::Argument { position, .. }) = gate_of(name) else { panic!("{name}") };
            let gate = setting_read_gate(name).expect(name);
            assert_eq!(gate.positions(), [usize::from(position)], "{name}");
            let facts = crate::param_facts(name).expect(name);
            let param = facts.param_names[usize::from(position)];
            assert!(matches!(param, "encoding" | "from_encoding"), "{name}");
        }
        let at = |name: &str| setting_read_gate(name).expect(name).positions();
        assert_eq!(at("mb_strlen"), [1]);
        assert_eq!(at("mb_str_pad"), [4]);
        assert_eq!(at("mb_convert_encoding"), [2], "the source encoding, not the target");
        assert_eq!(at("htmlspecialchars"), [0, 2]);
        assert_eq!(at("html_entity_decode"), [0, 2]);
        assert_eq!(at("get_html_translation_table"), [2]);
        assert_eq!(at("iconv_strrpos"), [1, 2]);
        assert_eq!(at("iconv_strlen"), [1]);
        for name in ACCESSORS {
            assert_eq!(at(name), [0], "{name}");
        }
    }

    /// Every gated name carries the cell's read on its row; the accessors and
    /// `mb_regex_set_options` carry the write beside it.
    #[test]
    fn the_rows_carry_the_cell_labels() {
        let names = PLAIN.iter().chain(SUBSTITUTING).chain(ICONV).copied().chain([
            "htmlspecialchars", "htmlentities", "html_entity_decode", "get_html_translation_table",
            "iconv_strrpos", "iconv_get_encoding", "mb_ereg", "mb_eregi", "mb_ereg_replace",
            "mb_eregi_replace", "mb_ereg_match", "mb_split",
        ]);
        for name in names {
            assert_eq!(effect_labels(name), Some(&[READ][..]), "{name}");
            if let Some(gate) = setting_read_gate(name) {
                assert_eq!(gate.cell(), SettingCell::Encoding, "{name}");
            }
        }
        for name in ACCESSORS.iter().chain(&["mb_regex_set_options"]) {
            assert_eq!(effect_labels(name), Some(&[READ, WRITE][..]), "{name}");
            assert_eq!(setting_read_gate(name).expect(name).cell(), SettingCell::Encoding);
        }
        assert_eq!(effect_labels("iconv_set_encoding"), Some(&[WRITE][..]));
        assert!(setting_read_gate("iconv_set_encoding").is_none());
        for name in ["mb_ereg", "mb_split", "mb_ereg_match"] {
            assert!(setting_read_gate(name).is_none(), "{name} reads on every call");
        }
        // The stateful search family, the callback form, the encoder of headers and the
        // detection reader are not coloured by this slice.
        for name in [
            "mb_ereg_search", "mb_ereg_search_init", "mb_ereg_replace_callback",
            "mb_encode_mimeheader", "mb_detect_encoding", "mb_get_info", "mb_http_input",
            "mb_convert_variables", "mb_preferred_mime_name", "iconv",
        ] {
            assert_eq!(effect_labels(name), None, "{name}");
        }
    }

    fn reads(name: &str, args: &[Option<GateArg<'_>>]) -> Option<bool> {
        setting_read_gate(name).expect(name).reads(args)
    }

    fn writes(name: &str, args: &[Option<GateArg<'_>>]) -> bool {
        setting_read_gate(name).expect(name).writes(args)
    }

    /// The plain class: omitted and `null` read, a literal name does not, anything else is
    /// undecided.
    #[test]
    fn a_plain_reader_reads_for_the_default_and_not_for_a_name() {
        use GateArg::{Bool, Int, NotText, Null, Omitted, Str};
        for name in PLAIN {
            let at = |arg| reads(name, &[arg]);
            assert_eq!(at(Some(Omitted)), Some(true), "{name}");
            assert_eq!(at(Some(Null)), Some(true), "{name}");
            assert_eq!(at(Some(Str("UTF-8"))), Some(false), "{name}");
            assert_eq!(at(Some(Str("auto"))), Some(false), "{name}: a ValueError, no read");
            assert_eq!(at(None), None, "{name}");
            for arg in [Int(1), Bool(true), NotText] {
                assert_eq!(at(Some(arg)), None, "{name}");
            }
        }
    }

    /// The substituting class reads for the default and is undecided for a name: the
    /// substitution character is the cell's, and an invalid subject reads it.
    #[test]
    fn a_substituting_reader_is_undecided_for_a_literal_name() {
        use GateArg::{Null, Omitted, Str};
        for name in SUBSTITUTING {
            assert_eq!(reads(name, &[Some(Omitted)]), Some(true), "{name}");
            assert_eq!(reads(name, &[Some(Null)]), Some(true), "{name}");
            assert_eq!(reads(name, &[Some(Str("UTF-8"))]), None, "{name}");
            assert_eq!(reads(name, &[None]), None, "{name}");
        }
    }

    /// `htmlspecialchars` and `htmlentities` return on an empty subject before the charset is
    /// looked at; `html_entity_decode` on one with no `&`; an empty charset names the default.
    #[test]
    fn the_html_readers_decide_by_their_subject_and_an_empty_charset() {
        use GateArg::{Null, Omitted, Str};
        for name in ["htmlspecialchars", "htmlentities"] {
            let at = |subject, charset| reads(name, &[subject, charset]);
            assert_eq!(at(Some(Str("a")), Some(Omitted)), Some(true), "{name}");
            assert_eq!(at(Some(Str("a")), Some(Null)), Some(true), "{name}");
            assert_eq!(at(Some(Str("a")), Some(Str(""))), Some(true), "{name}: '' is the default");
            assert_eq!(at(Some(Str("")), Some(Omitted)), Some(false), "{name}: returns first");
            assert_eq!(at(Some(Str("")), Some(Str(""))), Some(false), "{name}");
            assert_eq!(at(None, Some(Omitted)), None, "{name}: a subject the call hides");
            assert_eq!(at(None, Some(Str(""))), None, "{name}");
            let named = at(None, Some(Str("UTF-8")));
            assert_eq!(named, Some(false), "{name}: a name needs no subject");
            assert_eq!(at(Some(Str("a")), Some(Str("ISO-8859-1"))), Some(false), "{name}");
            assert_eq!(at(Some(Str("a")), None), None, "{name}");
        }
        let at = |subject, charset| reads("html_entity_decode", &[subject, charset]);
        assert_eq!(at(Some(Str("&amp;")), Some(Omitted)), Some(true));
        assert_eq!(at(Some(Str("plain")), Some(Omitted)), Some(false), "no & returns first");
        assert_eq!(at(Some(Str("plain")), Some(Str(""))), Some(false));
        assert_eq!(at(None, Some(Omitted)), None);
        assert_eq!(at(None, Some(Str("UTF-8"))), Some(false));
        // The table reads for the default, an empty name included, and has no subject.
        assert_eq!(reads("get_html_translation_table", &[Some(Omitted)]), Some(true));
        assert_eq!(reads("get_html_translation_table", &[Some(Str(""))]), Some(true));
        assert_eq!(reads("get_html_translation_table", &[Some(Str("UTF-8"))]), Some(false));
    }

    /// The iconv readers read for a `null` charset; the C library takes the locale's own charset
    /// for `''`, `char` and `locale`, so those are undecided; `iconv_strrpos` returns on an empty
    /// needle before it looks at the charset.
    #[test]
    fn the_iconv_readers_leave_the_locale_charsets_undecided() {
        use GateArg::{Null, Omitted, Str};
        for name in ICONV {
            let at = |arg| reads(name, &[arg]);
            assert_eq!(at(Some(Omitted)), Some(true), "{name}");
            assert_eq!(at(Some(Null)), Some(true), "{name}");
            assert_eq!(at(Some(Str("UTF-8"))), Some(false), "{name}");
            assert_eq!(at(Some(Str("ISO-8859-1//TRANSLIT"))), Some(false), "{name}");
            for locale in ["", "char", "CHAR", "Locale"] {
                assert_eq!(at(Some(Str(locale))), None, "{name} {locale:?}");
            }
        }
        let at = |needle, charset| reads("iconv_strrpos", &[needle, charset]);
        assert_eq!(at(Some(Str("b")), Some(Omitted)), Some(true));
        assert_eq!(at(Some(Str("")), Some(Omitted)), Some(false));
        assert_eq!(at(None, Some(Omitted)), None);
        assert_eq!(at(None, Some(Str("UTF-8"))), Some(false));
        assert_eq!(at(Some(Str("b")), Some(Str(""))), None);
    }

    /// An accessor reads with no argument or `null` and writes with anything else; the argument
    /// the call does not show is undecided for the read and keeps the write.
    #[test]
    fn an_accessor_reads_with_nothing_to_set_and_writes_otherwise() {
        use GateArg::{Bool, Int, NotText, Null, Omitted, Str};
        for name in ACCESSORS {
            for none in [Omitted, Null] {
                assert_eq!(reads(name, &[Some(none)]), Some(true), "{name}");
                assert!(!writes(name, &[Some(none)]), "{name}");
            }
            for set in [Str("UTF-8"), Int(65)] {
                assert_eq!(reads(name, &[Some(set)]), Some(false), "{name}");
                assert!(writes(name, &[Some(set)]), "{name}");
            }
            assert_eq!(reads(name, &[None]), None, "{name}");
            assert!(writes(name, &[None]), "{name}");
            // Not `null`, so it sets (and a bad value throws before it does).
            assert_eq!(reads(name, &[Some(Bool(true))]), Some(false), "{name}");
            assert_eq!(reads(name, &[Some(NotText)]), Some(false), "{name}");
        }
        // `mb_regex_set_options` hands back the previous options whatever it is given.
        for arg in [Omitted, Null, Str("i")] {
            assert_eq!(reads("mb_regex_set_options", &[Some(arg)]), Some(true));
        }
        assert_eq!(reads("mb_regex_set_options", &[None]), Some(true));
        assert!(!writes("mb_regex_set_options", &[Some(Omitted)]));
        assert!(!writes("mb_regex_set_options", &[Some(Null)]));
        assert!(writes("mb_regex_set_options", &[Some(Str("i"))]));
        assert!(writes("mb_regex_set_options", &[None]));
        // No other gate has a write to drop.
        assert!(writes("mb_strlen", &[Some(Omitted)]));
        assert!(writes("sort", &[Some(Omitted)]));
    }

    /// `iconv_get_encoding` reads for `all` and the three names it knows, whatever the case,
    /// and returns `false` for any other.
    #[test]
    fn iconv_get_encoding_reads_for_the_types_it_knows() {
        use GateArg::{Null, Omitted, Str};
        assert_eq!(reads("iconv_get_encoding", &[Some(Omitted)]), Some(true));
        for kind in ["all", "ALL", "input_encoding", "OUTPUT_ENCODING", "internal_encoding"] {
            assert_eq!(reads("iconv_get_encoding", &[Some(Str(kind))]), Some(true), "{kind}");
        }
        for kind in ["", "encoding", "internal"] {
            assert_eq!(reads("iconv_get_encoding", &[Some(Str(kind))]), Some(false), "{kind}");
        }
        assert_eq!(reads("iconv_get_encoding", &[Some(Null)]), None);
        assert_eq!(reads("iconv_get_encoding", &[None]), None);
    }
}
