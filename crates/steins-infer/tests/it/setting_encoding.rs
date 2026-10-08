//! The encoding cell's readers and writers in the effect lane (ADR-0101 §3.13, issue #1000, S6d):
//! a function that takes an `$encoding` reads the cell where the argument is omitted or `null`,
//! reads none where it is a literal name (or, for the functions that rebuild the string under
//! the substitution character, leaves it undecided), and is the `value-dependent-read` gap
//! otherwise; the accessors read with no argument and write with one; the mb-regex functions
//! read the cell on every call.
//!
//! The witness rows of `steins-catalog`'s `setting_encoding_oracle` decide each verdict against
//! PHP; these tests pin that the call site reaches it through every call shape.

use steins_infer::{Diagnostic, EFFECT_ID, EffectSummary, check, effect_summary};
use steins_syntax::SourceTree;

const READ: &str = "global.read.setting.encoding";
const WRITE: &str = "global.write.setting.encoding";
const DEPENDS: &str = "value-dependent-read";

fn summary(params: &str, body: &str) -> EffectSummary {
    let src = format!("<?php\nfunction f({params}) {{ {body} }}\n");
    let tree = SourceTree::parse(&src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == "f")
        .unwrap_or_else(|| panic!("no summary for f in {src}"))
}

fn has(s: &EffectSummary, label: &str) -> bool {
    s.labels.iter().any(|l| l == label)
}

fn findings(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

/// The call carries exactly `labels` and the body stays exhaustive.
fn proves(call: &str, labels: &[&str]) {
    let s = summary("", &format!("return {call};"));
    assert_eq!(s.labels, labels, "{call}: {s:?}");
    assert!(s.exhaustive && s.gaps.is_empty(), "{call}: {s:?}");
}

/// The call carries no label and the `value-dependent-read` gap.
fn depends(call: &str) {
    let s = summary("string $e, array $a", &format!("return {call};"));
    // The converters that count illegal characters keep their write whatever the read is.
    assert!(s.labels.iter().all(|l| l == WRITE), "{call}: {s:?}");
    assert!(!s.labels.iter().any(|l| l == READ), "{call}: {s:?}");
    assert!(s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    assert!(!s.exhaustive, "{call}: {s:?}");
}

/// A call to each of the plain readers, with the encoding named by `encoding` (`None` omits it).
/// The subject and the other arguments are literals, so only the encoding decides.
fn plain_calls(encoding: Option<&str>) -> Vec<String> {
    let e = |before: &str| encoding.map_or_else(String::new, |e| format!("{before}{e}"));
    vec![
        format!("mb_strlen('ab'{})", e(", ")),
        format!("mb_strwidth('ab'{})", e(", ")),
        format!("mb_strpos('ab', 'b', 0{})", e(", ")),
        format!("mb_strrpos('ab', 'b', 0{})", e(", ")),
        format!("mb_stripos('ab', 'B', 0{})", e(", ")),
        format!("mb_strripos('ab', 'B', 0{})", e(", ")),
        format!("mb_substr_count('ab', 'b'{})", e(", ")),
        format!("mb_strcut('ab', 0, 1{})", e(", ")),
        format!("mb_check_encoding('ab'{})", e(", ")),
        format!("mb_chr(65{})", e(", ")),
        format!("mb_ord('a'{})", e(", ")),
    ]
}

/// The functions that rebuild the string under the substitution character.
fn substituting_calls(encoding: Option<&str>) -> Vec<String> {
    let e = |before: &str| encoding.map_or_else(String::new, |e| format!("{before}{e}"));
    vec![
        format!("mb_substr('ab', 0, 1{})", e(", ")),
        format!("mb_strtoupper('ab'{})", e(", ")),
        format!("mb_strtolower('ab'{})", e(", ")),
        format!("mb_convert_case('ab', MB_CASE_UPPER{})", e(", ")),
        format!("mb_ucfirst('ab'{})", e(", ")),
        format!("mb_lcfirst('ab'{})", e(", ")),
        format!("mb_scrub('ab'{})", e(", ")),
        format!("mb_str_split('ab', 1{})", e(", ")),
        format!("mb_strimwidth('ab', 0, 1, ''{})", e(", ")),
        format!("mb_stristr('ab', 'B', false{})", e(", ")),
        format!("mb_strrchr('ab', 'b', false{})", e(", ")),
        format!("mb_strrichr('ab', 'B', false{})", e(", ")),
        format!("mb_strstr('ab', 'b', false{})", e(", ")),
        format!("mb_trim('ab', null{})", e(", ")),
        format!("mb_ltrim('ab', null{})", e(", ")),
        format!("mb_rtrim('ab', null{})", e(", ")),
        format!("mb_str_pad('ab', 4, ' ', STR_PAD_RIGHT{})", e(", ")),
        format!("mb_convert_kana('ab', 'KV'{})", e(", ")),
        format!("mb_convert_encoding('ab', 'UTF-16BE'{})", e(", ")),
        format!("mb_encode_numericentity('ab', [0x80, 0x10ffff, 0, 0xffffff]{})", e(", ")),
        format!("mb_decode_numericentity('ab', [0x80, 0x10ffff, 0, 0xffffff]{})", e(", ")),
    ]
}

/// An omitted encoding and a literal `null` are the proven read, through every function.
#[test]
fn an_omitted_or_null_encoding_is_the_proven_read() {
    // The two converters that count illegal characters write the cell beside the read.
    let labels = |call: &str| -> Vec<&'static str> {
        if call.starts_with("mb_convert_encoding") || call.starts_with("mb_scrub") {
            vec![READ, WRITE]
        } else {
            vec![READ]
        }
    };
    for call in plain_calls(None).into_iter().chain(substituting_calls(None)) {
        proves(&call, &labels(&call));
    }
    for call in plain_calls(Some("null")).into_iter().chain(substituting_calls(Some("null"))) {
        proves(&call, &labels(&call));
    }
    for call in [
        "iconv_strlen('ab')",
        "iconv_substr('ab', 0, 1)",
        "iconv_strpos('ab', 'b')",
        "iconv_strrpos('ab', 'b')",
        "iconv_mime_decode('ab')",
        "iconv_mime_decode_headers('ab')",
        "iconv_strlen('ab', null)",
        "htmlspecialchars('<')",
        "htmlentities('<')",
        "html_entity_decode('&lt;')",
        "get_html_translation_table()",
        "htmlspecialchars('<', ENT_QUOTES, null)",
        "htmlspecialchars('<', ENT_QUOTES, '')",
        "get_html_translation_table(HTML_ENTITIES, ENT_QUOTES, '')",
        "iconv_get_encoding()",
        "iconv_get_encoding('internal_encoding')",
    ] {
        proves(call, &[READ]);
    }
}

/// `mb_convert_encoding` and `mb_scrub` add to the illegal-character counter, which the
/// zero-argument `mb_check_encoding()` and `mb_get_info('illegal_chars')` read: the write is the
/// cell's, whatever the encoding shows, and the zero-argument reader carries the read.
#[test]
fn the_illegal_character_counter_is_the_cells() {
    proves("mb_check_encoding()", &[READ]);
    proves("mb_scrub('a')", &[READ, WRITE]);
    let s = summary("", "return mb_convert_encoding('a', 'UTF-16BE', 'UTF-8');");
    assert_eq!(s.labels, [WRITE], "{s:?}");
    assert!(s.gaps.contains(&DEPENDS), "{s:?}");
}

/// A literal encoding name reads nothing in the plain class; the substituting class leaves it
/// undecided, because the substitution character is the cell's and an invalid subject reads it.
#[test]
fn a_literal_name_reads_nothing_or_is_undecided_by_class() {
    for call in plain_calls(Some("'UTF-8'")) {
        proves(&call, &[]);
    }
    for call in substituting_calls(Some("'UTF-8'")) {
        let s = summary("", &format!("return {call};"));
        let written: &[&str] =
            if call.starts_with("mb_convert_encoding") || call.starts_with("mb_scrub") {
                &[WRITE]
            } else {
                &[]
            };
        assert_eq!(s.labels, written, "{call}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    }
    for call in [
        "iconv_strlen('ab', 'UTF-8')",
        "iconv_substr('ab', 0, 1, 'ISO-8859-1')",
        "iconv_strpos('ab', 'b', 0, 'UTF-8')",
        "iconv_strrpos('ab', 'b', 'UTF-8')",
        "iconv_mime_decode('ab', 0, 'UTF-8')",
        "iconv_mime_decode_headers('ab', 0, 'UTF-8')",
        "htmlspecialchars('<', ENT_QUOTES, 'UTF-8')",
        "htmlentities('<', ENT_QUOTES, 'ISO-8859-1')",
        "html_entity_decode('&lt;', ENT_QUOTES, 'UTF-8')",
        "get_html_translation_table(HTML_ENTITIES, ENT_QUOTES, 'UTF-8')",
        "iconv_get_encoding('no_such_type')",
        "iconv_strlen('ab', 'ISO-8859-1')",
    ] {
        proves(call, &[]);
    }
    // The C library takes the locale's own charset for these names.
    for call in [
        "iconv_strlen('ab', '')",
        "iconv_strlen('ab', 'char')",
        "iconv_substr('ab', 0, 1, 'locale')",
        "iconv_strlen('ab', 'ASCII//TRANSLIT')",
        "iconv_substr('ab', 0, 1, 'UTF-8//IGNORE')",
        "iconv_strpos('ab', 'b', 0, 'ascii//translit//ignore')",
        "iconv_strrpos('ab', 'b', 'ASCII//TRANSLIT')",
        "iconv_mime_decode('ab', 0, 'ASCII//TRANSLIT')",
        "iconv_mime_decode_headers('ab', 0, 'ASCII//TRANSLIT')",
    ] {
        depends(call);
    }
}

/// `htmlspecialchars` and `htmlentities` return on an empty subject, and `html_entity_decode` on
/// one with no `&`, before the charset is looked at (`ext/standard/html.c`): the subject decides.
#[test]
fn the_html_readers_return_before_the_charset_on_an_empty_subject() {
    for call in [
        "htmlspecialchars('')",
        "htmlentities('')",
        "htmlspecialchars('', ENT_QUOTES, '')",
        "html_entity_decode('plain')",
        "html_entity_decode('')",
        "html_entity_decode('plain', ENT_QUOTES, '')",
        "iconv_strrpos('ab', '')",
    ] {
        proves(call, &[]);
    }
    for call in [
        "htmlspecialchars($s)",
        "htmlentities($s)",
        "html_entity_decode($s)",
        "iconv_strrpos('ab', $s)",
    ] {
        let s = summary("string $s", &format!("return {call};"));
        assert!(s.labels.is_empty() && s.gaps.contains(&DEPENDS), "{call}: {s:?}");
    }
    // A name needs no subject.
    let s = summary("string $s", "return htmlspecialchars($s, ENT_QUOTES, 'UTF-8');");
    assert!(s.labels.is_empty() && !s.gaps.contains(&DEPENDS), "{s:?}");
}

/// An encoding the site cannot read is the gap and no label, in every call shape.
#[test]
fn an_encoding_the_site_cannot_read_is_the_gap() {
    for call in [
        "mb_strlen('ab', $e)",
        "mb_strlen('ab', $e . '-8')",
        "mb_strlen('ab', \"{$e}\")",
        "mb_strlen('ab', self::ENCODING)",
        "mb_strlen('ab', ENC)",
        "mb_strlen('ab', 5)",
        "mb_strlen('ab', true)",
        "mb_strlen('ab', encoding: 'UTF-8')",
        "mb_strlen(...$a)",
        "mb_strpos('ab', 'b', 0, $e)",
        "mb_str_pad('ab', 4, ' ', STR_PAD_RIGHT, $e)",
        "mb_substr('ab', 0, 1, $e)",
        "mb_convert_encoding('ab', 'UTF-8', $e)",
        "mb_convert_encoding('ab', 'UTF-8', $a)",
        "iconv_strlen('ab', $e)",
        "htmlspecialchars('<', ENT_QUOTES, $e)",
        "htmlentities('<', ENT_QUOTES, $e)",
        "html_entity_decode('&', ENT_QUOTES, $e)",
        "get_html_translation_table(HTML_ENTITIES, ENT_QUOTES, $e)",
        "iconv_get_encoding($e)",
    ] {
        depends(call);
    }
}

/// An encoding at a position the call does not reach is the default, however many arguments
/// precede it.
#[test]
fn an_encoding_further_along_is_read_at_its_position() {
    proves("mb_strpos('ab', 'b', 1)", &[READ]);
    proves("mb_strpos('ab', 'b', 1, 'UTF-8')", &[]);
    proves("mb_str_pad('ab', 4)", &[READ]);
    proves("mb_str_pad('ab', 4, ' ', STR_PAD_LEFT, null)", &[READ]);
    proves("mb_strwidth('ab', 'UTF-8')", &[]);
    // `mb_convert_encoding`'s encoding is the third (the source), not the second.
    proves("mb_convert_encoding('ab', 'UTF-16BE')", &[READ, WRITE]);
    depends("mb_convert_encoding('ab', 'UTF-16BE', 'UTF-8')");
}

/// The accessors read with no argument (or `null`) and write with one; a value the site cannot
/// read keeps the write and is the gap for the read.
#[test]
fn the_accessors_read_with_no_argument_and_write_with_one() {
    for name in [
        "mb_internal_encoding",
        "mb_regex_encoding",
        "mb_http_output",
        "mb_detect_order",
        "mb_language",
        "mb_substitute_character",
    ] {
        proves(&format!("{name}()"), &[READ]);
        proves(&format!("{name}(null)"), &[READ]);
        proves(&format!("{name}('UTF-8')"), &[WRITE]);
        let s = summary("string $e", &format!("return {name}($e);"));
        assert_eq!(s.labels, [WRITE], "{name}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{name}: {s:?}");
    }
    proves("mb_substitute_character(65)", &[WRITE]);
    proves("mb_substitute_character('none')", &[WRITE]);
    proves("mb_regex_encoding('UTF-8')", &[WRITE]);
    proves("mb_detect_order(['UTF-8', 'ASCII'])", &[WRITE]);
    // `mb_regex_set_options` hands back the previous options, so it reads on every call.
    proves("mb_regex_set_options()", &[READ]);
    proves("mb_regex_set_options(null)", &[READ]);
    proves("mb_regex_set_options('i')", &[READ, WRITE]);
    let s = summary("string $o", "return mb_regex_set_options($o);");
    assert!(has(&s, READ) && has(&s, WRITE), "{s:?}");
    assert!(!s.gaps.contains(&DEPENDS), "{s:?}");
    // `iconv_set_encoding` writes an ini entry, whatever it is given.
    proves("iconv_set_encoding('internal_encoding', 'UTF-8')", &[WRITE]);
}

/// The mb-regex functions compile under the cell's encoding and options on every call.
#[test]
fn the_regex_functions_read_the_cell_on_every_call() {
    for call in [
        "mb_ereg('a', 'b')",
        "mb_eregi('a', 'b')",
        "mb_ereg_replace('a', 'c', 'b')",
        "mb_eregi_replace('a', 'c', 'b')",
        "mb_ereg_match('a', 'b')",
        "mb_split('a', 'b')",
        "mb_ereg_replace('a', 'c', 'b', 'i')",
    ] {
        proves(call, &[READ]);
    }
    let s = summary("$m", "return mb_ereg('a', 'b', $m);");
    assert!(has(&s, READ), "{s:?}");
    // The search family keeps its state in the interpreter, and the callback form runs user
    // code: neither is coloured here.
    for call in [
        "mb_ereg_search('a')",
        "mb_ereg_search_init('a')",
        "mb_ereg_replace_callback('a', fn ($m) => 'x', 'b')",
    ] {
        let s = summary("", &format!("return {call};"));
        assert!(!has(&s, READ) && !s.exhaustive, "{call}: {s:?}");
    }
}

/// The five ini names that reset the mb-regex encoding are the cell's, and `ini_get` of them is
/// its read.
#[test]
fn the_ini_names_that_reset_the_regex_encoding_are_the_cells() {
    for name in [
        "default_charset",
        "internal_encoding",
        "input_encoding",
        "output_encoding",
        "mbstring.internal_encoding",
    ] {
        proves(&format!("ini_get('{name}')"), &[READ]);
        proves(&format!("ini_set('{name}', 'ISO-8859-1')"), &[READ, WRITE]);
        proves(&format!("ini_restore('{name}')"), &[WRITE]);
    }
}

/// A reader handed over as a callback is called with arguments of the invoker's choosing.
#[test]
fn an_encoding_reader_handed_over_as_a_callback_is_the_gap() {
    for callee in ["mb_strtolower", "mb_strlen", "htmlspecialchars", "iconv_strlen"] {
        let s = summary("array $a", &format!("return array_map('{callee}', $a);"));
        assert!(!s.labels.iter().any(|l| l == READ), "{callee}: {s:?}");
        assert!(s.gaps.contains(&DEPENDS), "{callee}: {s:?}");
    }
    // A function with no gate and no row of its own is still the `no-effect-row` gap.
    let s = summary("", "return mb_detect_encoding('a');");
    assert!(s.labels.is_empty() && !s.exhaustive && !s.gaps.contains(&DEPENDS), "{s:?}");
}

/// A row on a function that was `no-effect-row` does not make a body that calls something unknown
/// look exhaustive: the unknown call keeps its own gap beside the proven read.
#[test]
fn a_new_row_leaves_an_unknown_call_beside_it_open() {
    let s = summary("", "mb_strlen('a'); return mb_detect_encoding('a');");
    assert_eq!(s.labels, [READ], "{s:?}");
    assert!(!s.exhaustive, "{s:?}");
    let s = summary("", "mb_strlen('a'); return unknown_function('a');");
    assert_eq!(s.labels, [READ], "{s:?}");
    assert!(!s.exhaustive, "{s:?}");
}

/// A body that holds a proven read carries it up to its callers.
#[test]
fn the_read_propagates_to_a_caller() {
    let src = "<?php\nfunction reader() { return mb_strlen('ab'); }\n\
               function named() { return mb_strlen('ab', 'UTF-8'); }\n\
               function top() { return reader() + named(); }\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    let summaries = effect_summary(&tree, &functions, &classes);
    let of = |name: &str| summaries.iter().find(|s| s.symbol == name).expect(name);
    assert_eq!(of("reader").labels, [READ]);
    assert!(of("named").labels.is_empty() && of("named").exhaustive);
    assert_eq!(of("top").labels, [READ], "{:?}", of("top"));
}

/// An envelope that does not admit the read is exceeded at the call with a proven read; a literal
/// name, a gap and an envelope that admits the cell are not (ADR-0101 §3.5).
#[test]
fn a_pure_envelope_over_an_encoding_read_is_exceeded() {
    let pure = |body: &str| {
        format!("<?php\n#[\\Steins\\Pure]\nfunction f(string $e): int|false {{ return {body}; }}\n")
    };
    let d = findings(&pure("mb_strlen('ab')"));
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains(READ), "{}", d[0].message);
    for body in
        ["mb_strlen('ab', 'UTF-8')", "mb_strlen('ab', $e)", "mb_strpos('ab', 'b', 0, 'UTF-8')"]
    {
        let d = findings(&pure(body));
        assert!(d.is_empty(), "{body}: {d:#?}");
    }
    let admits = "<?php\n#[\\Steins\\Effect('global.read.setting')]\n\
                  function f(): int|false { return mb_strlen('ab'); }\n";
    assert!(findings(admits).is_empty());
    let write = "<?php\n#[\\Steins\\Pure]\n\
                 function f(): bool { return mb_internal_encoding('UTF-8'); }\n";
    let d = findings(write);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains(WRITE), "{}", d[0].message);
}
