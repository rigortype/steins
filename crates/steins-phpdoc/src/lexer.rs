//! A hand-written lexer reproducing phpstan/phpdoc-parser's `Lexer` token
//! stream for the type-expression grammar (ADR-0029).
//!
//! The reference lexer is a single anchored, case-insensitive PCRE alternation
//! whose branch order decides ties. This lexer applies the same matchers in the
//! same order at each position, so it produces the same tokens — including the
//! whitespace and comment trivia the parser depends on (`array {` vs `array{`,
//! offset-access vs `[]`, multi-line array shapes).
//!
//! Bytes that no branch matches (e.g. `\f`, `\v`) are skipped, exactly as
//! `preg_match_all` drops unmatched input. A final [`TokenKind::End`] is appended.

/// Lexical token kinds. A subset of the reference lexer's tokens — the ones the
/// type grammar reaches. Ordering here has no meaning; matcher precedence lives
/// in [`tokenize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Reference,
    Union,
    Intersection,
    Nullable,
    Negated,
    OpenParen,
    CloseParen,
    OpenAngle,
    CloseAngle,
    OpenSquare,
    CloseSquare,
    OpenCurly,
    CloseCurly,
    Comma,
    Comment,
    Variadic,
    DoubleColon,
    DoubleArrow,
    Arrow,
    Equal,
    Colon,
    ClosePhpdoc,
    PhpdocEol,
    HorizontalWs,
    Float,
    Integer,
    SingleQuotedString,
    DoubleQuotedString,
    Identifier,
    ThisVariable,
    Variable,
    Wildcard,
    Other,
    End,
}

/// One lexed token: its kind, its source text, and its byte offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub value: String,
    pub start: u32,
    pub end: u32,
    pub line: u32,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b >= 0x80
}

fn is_var_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_var_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b)
}

/// Tokenize a type-expression string into the reference token stream.
pub fn tokenize(input: &str) -> Vec<Token> {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut tokens = Vec::new();
    let mut pos = 0usize;
    let mut line = 1u32;

    while pos < len {
        let (kind, end) = match match_token(bytes, pos) {
            Some(m) => m,
            None => {
                // Unmatched byte (e.g. \f, \v): skip it, like preg_match_all.
                pos += 1;
                continue;
            }
        };
        let value = String::from_utf8_lossy(&bytes[pos..end]).into_owned();
        let tok = Token {
            kind,
            value,
            start: pos as u32,
            end: end as u32,
            line,
        };
        if kind == TokenKind::PhpdocEol {
            line += 1;
        }
        tokens.push(tok);
        pos = end;
    }

    tokens.push(Token {
        kind: TokenKind::End,
        value: String::new(),
        start: len as u32,
        end: len as u32,
        line,
    });
    tokens
}

/// Try every matcher, in reference branch order, at `pos`. Returns the winning
/// `(kind, end)` or `None` if no branch matches this byte.
fn match_token(b: &[u8], pos: usize) -> Option<(TokenKind, usize)> {
    if let Some(e) = match_horizontal_ws(b, pos) {
        return Some((TokenKind::HorizontalWs, e));
    }
    if let Some(e) = match_identifier(b, pos) {
        return Some((TokenKind::Identifier, e));
    }
    if let Some(e) = match_this_variable(b, pos) {
        return Some((TokenKind::ThisVariable, e));
    }
    if let Some(e) = match_variable(b, pos) {
        return Some((TokenKind::Variable, e));
    }
    if let Some(e) = match_reference(b, pos) {
        return Some((TokenKind::Reference, e));
    }
    // Single-char structural tokens (Union/Intersection/… ) that come before
    // the multi-char and literal matchers.
    match b[pos] {
        b'|' => return Some((TokenKind::Union, pos + 1)),
        b'&' => return Some((TokenKind::Intersection, pos + 1)),
        b'?' => return Some((TokenKind::Nullable, pos + 1)),
        b'!' => return Some((TokenKind::Negated, pos + 1)),
        b'(' => return Some((TokenKind::OpenParen, pos + 1)),
        b')' => return Some((TokenKind::CloseParen, pos + 1)),
        b'<' => return Some((TokenKind::OpenAngle, pos + 1)),
        b'>' => return Some((TokenKind::CloseAngle, pos + 1)),
        b'[' => return Some((TokenKind::OpenSquare, pos + 1)),
        b']' => return Some((TokenKind::CloseSquare, pos + 1)),
        b'{' => return Some((TokenKind::OpenCurly, pos + 1)),
        b'}' => return Some((TokenKind::CloseCurly, pos + 1)),
        b',' => return Some((TokenKind::Comma, pos + 1)),
        _ => {}
    }
    if let Some(e) = match_comment(b, pos) {
        return Some((TokenKind::Comment, e));
    }
    if starts_with(b, pos, b"...") {
        return Some((TokenKind::Variadic, pos + 3));
    }
    if starts_with(b, pos, b"::") {
        return Some((TokenKind::DoubleColon, pos + 2));
    }
    if starts_with(b, pos, b"=>") {
        return Some((TokenKind::DoubleArrow, pos + 2));
    }
    if starts_with(b, pos, b"->") {
        return Some((TokenKind::Arrow, pos + 2));
    }
    if b[pos] == b'=' {
        return Some((TokenKind::Equal, pos + 1));
    }
    if b[pos] == b':' {
        return Some((TokenKind::Colon, pos + 1));
    }
    if starts_with(b, pos, b"*/") {
        return Some((TokenKind::ClosePhpdoc, pos + 2));
    }
    if let Some(e) = match_phpdoc_eol(b, pos) {
        return Some((TokenKind::PhpdocEol, e));
    }
    if let Some(e) = match_float(b, pos) {
        return Some((TokenKind::Float, e));
    }
    if let Some(e) = match_integer(b, pos) {
        return Some((TokenKind::Integer, e));
    }
    if let Some(e) = match_quoted(b, pos, b'\'') {
        return Some((TokenKind::SingleQuotedString, e));
    }
    if let Some(e) = match_quoted(b, pos, b'"') {
        return Some((TokenKind::DoubleQuotedString, e));
    }
    if b[pos] == b'*' {
        return Some((TokenKind::Wildcard, pos + 1));
    }
    if let Some(e) = match_other(b, pos) {
        return Some((TokenKind::Other, e));
    }
    None
}

fn starts_with(b: &[u8], pos: usize, needle: &[u8]) -> bool {
    b.len() >= pos + needle.len() && &b[pos..pos + needle.len()] == needle
}

fn match_horizontal_ws(b: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    (i > pos).then_some(i)
}

/// `(?:\\?[a-z_\x80-\xFF][0-9a-z_\x80-\xFF-]*)+` (case-insensitive).
fn match_identifier(b: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos;
    let mut matched = false;
    loop {
        let mut k = i;
        if k < b.len() && b[k] == b'\\' {
            k += 1;
        }
        if k < b.len() && is_ident_start(b[k]) {
            k += 1;
            while k < b.len() && is_ident_cont(b[k]) {
                k += 1;
            }
            i = k;
            matched = true;
        } else {
            break;
        }
    }
    matched.then_some(i)
}

/// `\$this(?![0-9a-z_\x80-\xFF])` (case-insensitive).
fn match_this_variable(b: &[u8], pos: usize) -> Option<usize> {
    let kw = b"$this";
    if !starts_with_ci(b, pos, kw) {
        return None;
    }
    let end = pos + kw.len();
    if end < b.len() && is_var_cont(b[end]) {
        return None;
    }
    Some(end)
}

/// `\$[a-z_\x80-\xFF][0-9a-z_\x80-\xFF]*` (case-insensitive).
fn match_variable(b: &[u8], pos: usize) -> Option<usize> {
    if b[pos] != b'$' {
        return None;
    }
    let mut i = pos + 1;
    if i >= b.len() || !is_var_start(b[i]) {
        return None;
    }
    i += 1;
    while i < b.len() && is_var_cont(b[i]) {
        i += 1;
    }
    Some(i)
}

/// `&` when followed (after optional whitespace) by `.`, `,`, `=`, `)`, or a
/// `$variable` that is not `$this` — the reference's by-reference disambiguation.
fn match_reference(b: &[u8], pos: usize) -> Option<usize> {
    if b[pos] != b'&' {
        return None;
    }
    let mut i = pos + 1;
    while i < b.len() && is_ws(b[i]) {
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    match b[i] {
        b'.' | b',' | b'=' | b')' => Some(pos + 1),
        b'$' => {
            // `$` not beginning `$this` + boundary.
            if starts_with_ci(b, i, b"$this") {
                let after = i + 5;
                let is_this = after >= b.len() || !is_var_cont(b[after]);
                if is_this {
                    return None;
                }
            }
            Some(pos + 1)
        }
        _ => None,
    }
}

/// `//[^\r\n]*(?=\n|\r|\*/)` — a line comment that must be followed by a newline
/// or `*/`.
fn match_comment(b: &[u8], pos: usize) -> Option<usize> {
    if !starts_with(b, pos, b"//") {
        return None;
    }
    let mut i = pos + 2;
    while i < b.len() {
        if b[i] == b'\n' || b[i] == b'\r' {
            return Some(i); // lookahead \n|\r satisfied
        }
        if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
            return Some(i); // lookahead \*/ satisfied
        }
        i += 1;
    }
    None
}

/// `\r?\n[\t ]*(?:\*(?!/)\x20?)?`.
fn match_phpdoc_eol(b: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos;
    if i < b.len() && b[i] == b'\r' {
        i += 1;
    }
    if i >= b.len() || b[i] != b'\n' {
        return None;
    }
    i += 1;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i < b.len() && b[i] == b'*' && !(i + 1 < b.len() && b[i + 1] == b'/') {
        i += 1;
        if i < b.len() && b[i] == b' ' {
            i += 1;
        }
    }
    Some(i)
}

fn digits_with_underscores(b: &[u8], mut i: usize, is_digit: fn(u8) -> bool) -> usize {
    // [d]+ (_[d]+)*
    while i < b.len() && is_digit(b[i]) {
        i += 1;
    }
    loop {
        if i < b.len() && b[i] == b'_' && i + 1 < b.len() && is_digit(b[i + 1]) {
            i += 1;
            while i < b.len() && is_digit(b[i]) {
                i += 1;
            }
        } else {
            break;
        }
    }
    i
}

fn dec(x: u8) -> bool {
    x.is_ascii_digit()
}

/// The float pattern (see reference regexp). Tried before integer.
fn match_float(b: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let after_sign = i;

    // Helper closures operate on absolute index.
    let has_digit = |x: usize| x < b.len() && dec(b[x]);

    // Case A: digits+ (.) digits* (exp)?    — requires a dot
    // Case B: digits* (.) digits+ (exp)?    — requires a dot
    // Case C: digits+ exp                   — requires exp, no dot
    // Try A/B (with dot) first, then C.
    // Parse an optional leading integer part.
    let int_end = if has_digit(after_sign) {
        digits_with_underscores(b, after_sign, dec)
    } else {
        after_sign
    };

    // With a dot:
    if int_end < b.len() && b[int_end] == b'.' {
        let mut j = int_end + 1;
        let frac_present = has_digit(j);
        if frac_present {
            j = digits_with_underscores(b, j, dec);
        }
        // Need at least one digit somewhere around the dot.
        let int_present = int_end > after_sign;
        if int_present || frac_present {
            // optional exponent
            j = match_exponent(b, j);
            return Some(j);
        }
        return None;
    }

    // No dot: Case C requires exponent immediately after an integer part.
    if int_end > after_sign
        && let Some(j) = try_exponent(b, int_end)
    {
        return Some(j);
    }
    None
}

/// Consume an optional `e[+-]?digits` exponent, returning the new index.
fn match_exponent(b: &[u8], i: usize) -> usize {
    try_exponent(b, i).unwrap_or(i)
}

/// Consume a required `e[+-]?digits(_digits)*` exponent; `None` if absent.
fn try_exponent(b: &[u8], i: usize) -> Option<usize> {
    if i >= b.len() || (b[i] != b'e' && b[i] != b'E') {
        return None;
    }
    let mut j = i + 1;
    if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
        j += 1;
    }
    if j >= b.len() || !dec(b[j]) {
        return None;
    }
    j = digits_with_underscores(b, j, dec);
    Some(j)
}

/// The integer pattern: binary / octal / hex / decimal, all with `_` groups.
fn match_integer(b: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let start = i;
    // `0b`/`0o`/`0x` prefixes, case-insensitive on the base letter (the `i` flag).
    let two = |c: u8| b.len() > start + 1 && b[start] == b'0' && b[start + 1].eq_ignore_ascii_case(&c);

    if two(b'b') {
        let j = digits_with_underscores(b, start + 2, |x| x == b'0' || x == b'1');
        return (j > start + 2).then_some(j);
    }
    if two(b'o') {
        let j = digits_with_underscores(b, start + 2, |x| (b'0'..=b'7').contains(&x));
        return (j > start + 2).then_some(j);
    }
    if two(b'x') {
        let j = digits_with_underscores(b, start + 2, |x| x.is_ascii_hexdigit());
        return (j > start + 2).then_some(j);
    }
    if i < b.len() && dec(b[i]) {
        let j = digits_with_underscores(b, i, dec);
        return Some(j);
    }
    None
}

/// A single- or double-quoted string with `\`-escapes (no raw newlines).
fn match_quoted(b: &[u8], pos: usize, quote: u8) -> Option<usize> {
    if b[pos] != quote {
        return None;
    }
    let mut i = pos + 1;
    while i < b.len() {
        let c = b[i];
        if c == b'\\' {
            if i + 1 < b.len() && b[i + 1] != b'\r' && b[i + 1] != b'\n' {
                i += 2;
                continue;
            }
            return None;
        }
        if c == quote {
            return Some(i + 1);
        }
        if c == b'\r' || c == b'\n' {
            return None;
        }
        i += 1;
    }
    None
}

/// `(?:(?!\*/)[^\s])+` — a run of non-whitespace not starting `*/`.
fn match_other(b: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos;
    while i < b.len() {
        if is_ws(b[i]) {
            break;
        }
        if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
            break;
        }
        i += 1;
    }
    (i > pos).then_some(i)
}

fn starts_with_ci(b: &[u8], pos: usize, needle: &[u8]) -> bool {
    if b.len() < pos + needle.len() {
        return false;
    }
    b[pos..pos + needle.len()].eq_ignore_ascii_case(needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(s: &str) -> Vec<TokenKind> {
        tokenize(s).into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn simple_identifier() {
        assert_eq!(kinds("int"), vec![TokenKind::Identifier, TokenKind::End]);
    }

    #[test]
    fn fqcn_is_one_identifier() {
        let toks = tokenize("\\Foo\\Bar\\Baz");
        assert_eq!(toks[0].kind, TokenKind::Identifier);
        assert_eq!(toks[0].value, "\\Foo\\Bar\\Baz");
    }

    #[test]
    fn reference_vs_intersection() {
        // `&` before `$a` (a variable) is a by-reference marker...
        assert_eq!(tokenize("&$a")[0].kind, TokenKind::Reference);
        // ...but `&` before an identifier is an intersection.
        assert_eq!(tokenize("&B")[0].kind, TokenKind::Intersection);
        // `&` before `$this` is an intersection, not a reference.
        assert_eq!(tokenize("&$this")[0].kind, TokenKind::Intersection);
    }

    #[test]
    fn float_before_integer() {
        assert_eq!(tokenize("123.2")[0].kind, TokenKind::Float);
        assert_eq!(tokenize("123")[0].kind, TokenKind::Integer);
        assert_eq!(tokenize("0x1f")[0].kind, TokenKind::Integer);
    }

    #[test]
    fn leading_underscore_is_identifier() {
        assert_eq!(tokenize("_123")[0].kind, TokenKind::Identifier);
    }

    #[test]
    fn horizontal_ws_and_eol() {
        // A leading `int ` is one identifier + horizontal whitespace...
        let toks = tokenize("int x");
        assert_eq!(toks[0].kind, TokenKind::Identifier);
        assert_eq!(toks[1].kind, TokenKind::HorizontalWs);
        assert_eq!(toks[2].kind, TokenKind::Identifier);
        // ...but a newline (with the next line's leading indent) is one
        // PhpdocEol token, exactly as the reference lexer folds them.
        let toks = tokenize("a\n b");
        assert_eq!(toks[0].kind, TokenKind::Identifier);
        assert_eq!(toks[1].kind, TokenKind::PhpdocEol);
        assert_eq!(toks[1].value, "\n ");
        assert_eq!(toks[2].kind, TokenKind::Identifier);
    }
}
