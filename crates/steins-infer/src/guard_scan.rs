//! The syntactic guard rule of the Asserted all-rejected rows (issue #1099).
//!
//! An `Asserted` fact is a claim: the docblock says the variable holds `'a'|'b'`, and a
//! condition the walk cannot decide (`in_array($x, ['a', 'b'], true)`, `strlen($x) === 1`,
//! a stored boolean, a by-reference helper) may already have removed every one of those
//! values before the call. PHP then never reaches the call with an allowed value, and the
//! "every member is rejected" claim is about a path that does not exist.
//!
//! No path analysis decides that, so the rows take a conservative **syntactic** rule over the
//! enclosing function body. The *subject* is the reported variable (or an offset's base)
//! and its **copy group**: every variable a plain `$a = $b;` (or a ternary arm of
//! `$a = c ? $b : d;`) joins to a member, transitively, anywhere in the body. The row
//! declines when any member occurs anywhere in the body other than as:
//!
//! * (a) a plain copy source or target (`$a = $b;`, a ternary arm), or the target of a plain
//!   assignment (a write, not a read);
//! * (b) the reported argument or return itself;
//! * (c) an operand compared with the standalone keyword `null` by `===` or `!==`;
//! * (d) the out-parameter position of `preg_match` / `preg_match_all`, the seed position.
//!
//! Any other occurrence (a condition, a call argument, `isset`, `??`, a stored boolean, a
//! by-reference helper, a closure `use`, an interpolation, an `echo`) declines. So does a body
//! that mentions a construct reading variables by name or by reference (`func_get_args`,
//! `get_defined_vars`, `compact`, `extract`, `$$`, `${`, `$GLOBALS`, `&$`, `eval`, `include`,
//! `global`), and a function whose body cannot be found (a closure, the top level).
//! Over-silencing a new finding is acceptable.
//!
//! The scan works on bytes and compares with `ends_with`/`starts_with`, never on a computed
//! `&str` index, so a multibyte character anywhere in the body cannot panic it. Each body is
//! scanned once per thread and cached (see [`BodyScan`]); a row then costs a lookup.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use steins_syntax::{ArgValue, Span};

use crate::cx::Cx;

/// A construct that reads or writes variables by name or by reference.
const POISON: &[&str] = &[
    "func_get_arg",
    "get_defined_vars",
    "debug_backtrace",
    "compact",
    "extract",
    "$$",
    "${",
    "$GLOBALS",
    "&$",
    "eval",
    "include",
    "require",
    "global ",
];

/// One `$name` of a body: its name and the byte range of the `$name` token, body-relative.
struct Occurrence {
    name: String,
    start: usize,
    end: usize,
}

/// A body scanned once: its `$name` tokens, its copy edges, and the answers already given.
pub(crate) struct BodyScan {
    text: String,
    occurrences: Vec<Occurrence>,
    edges: HashMap<String, Vec<String>>,
    /// Every plain copy `(target, source, byte of the source occurrence)`.
    copies: Vec<(String, String, usize)>,
    poisoned: bool,
    /// Per subject: the (at most two) occurrences of its copy group no rule admits, each
    /// with the body-relative range `[lo, hi]` of a reported use that would be exactly it.
    disallowed: RefCell<HashMap<String, Rc<Ranges>>>,
}

/// Body-relative ranges of reported-use offsets.
type Ranges = Vec<(usize, usize)>;

/// The scans of one file walk's bodies, keyed by the file and the body span: a body is
/// scanned once however many rows read it ([`Cx::guard_scans`]).
///
/// [`Cx::guard_scans`]: crate::cx::Cx::guard_scans
pub(crate) type GuardScans = RefCell<HashMap<(usize, u32, u32), Rc<BodyScan>>>;

/// Whether the Asserted all-rejected row for `value`, reported at byte `offset`, declines.
/// `false` for a value that is not a variable (a call result has no guard to read).
pub(crate) fn declines(cx: &Cx, value: &ArgValue, offset: u32) -> bool {
    let subject = match value {
        ArgValue::Var(n) => n.as_str(),
        ArgValue::OffsetRead { base, .. } => match &**base {
            ArgValue::Var(n) => n.as_str(),
            _ => return false,
        },
        _ => return false,
    };
    let Some(body) = enclosing_body(cx, offset) else { return true };
    let Some(scan) = scan_of(cx, body) else { return true };
    if scan.poisoned {
        return true;
    }
    let rel = (offset - body.start) as usize;
    scan.disallowed_of(subject).iter().any(|&(lo, hi)| !(lo <= rel && rel <= hi))
}

/// The innermost function or method body span containing `offset`.
fn enclosing_body(cx: &Cx, offset: u32) -> Option<Span> {
    let tree = cx.tree();
    let inside = |s: &Span| s.start <= offset && offset < s.end;
    let functions = tree.functions().iter().map(|f| f.body_span);
    let methods = tree.classes().iter().flat_map(|c| c.methods.iter()).filter_map(|m| m.body_span);
    functions.chain(methods).filter(|s| inside(s)).min_by_key(|s| s.end - s.start)
}

fn scan_of(cx: &Cx, body: Span) -> Option<Rc<BodyScan>> {
    let text = cx.tree().source_slice(body)?;
    let Some(cache) = cx.guard_scans else { return Some(Rc::new(BodyScan::new(text))) };
    let scan = cache
        .borrow_mut()
        .entry((cx.cur, body.start, body.end))
        .or_insert_with(|| Rc::new(BodyScan::new(text)))
        .clone();
    Some(scan)
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// The index just past the last non-whitespace byte before `i`.
fn prev_end(b: &[u8], mut i: usize) -> usize {
    while i > 0 && b[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    i
}

/// The index of the first non-whitespace byte at or after `i`.
fn next_start(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn ends_with_at(b: &[u8], end: usize, pat: &[u8]) -> bool {
    end >= pat.len() && b[end - pat.len()..end].eq_ignore_ascii_case(pat)
}

fn starts_with_at(b: &[u8], at: usize, pat: &[u8]) -> bool {
    b.get(at..at + pat.len()).is_some_and(|w| w.eq_ignore_ascii_case(pat))
}

/// A word `pat` ends at `end` and is not the tail of a longer name, a variable or a member.
fn word_ends_at(b: &[u8], end: usize, pat: &[u8]) -> bool {
    ends_with_at(b, end, pat)
        && (end == pat.len() || {
            let c = b[end - pat.len() - 1];
            !(is_ident(c) || matches!(c, b'$' | b'>' | b':' | b'\\'))
        })
}

/// A word `pat` starts at `at` and is not the head of a longer name.
fn word_starts_at(b: &[u8], at: usize, pat: &[u8]) -> bool {
    starts_with_at(b, at, pat) && !b.get(at + pat.len()).is_some_and(|&c| is_ident(c))
}

/// The `lhs` of a plain assignment `$lhs =` that ends at `end` (just past the `=`).
fn assign_target(b: &[u8], end: usize) -> Option<String> {
    if end == 0 || b[end - 1] != b'=' {
        return None;
    }
    if end >= 2 && b"=!<>.+-*/%&|^?".contains(&b[end - 2]) {
        return None;
    }
    let e = prev_end(b, end - 1);
    let mut s = e;
    while s > 0 && is_ident(b[s - 1]) {
        s -= 1;
    }
    if s == e || s == 0 || b[s - 1] != b'$' || (s >= 2 && matches!(b[s - 2], b':' | b'>' | b'$')) {
        return None;
    }
    String::from_utf8(b[s..e].to_vec()).ok()
}

/// The variable the statement-level `$lhs = ... ? $v : ...;` containing the byte `at` assigns.
fn ternary_target(b: &[u8], at: usize) -> Option<String> {
    let from = b[..at].iter().rposition(|c| matches!(c, b';' | b'{' | b'}')).map_or(0, |p| p + 1);
    let s = next_start(b, from);
    if b.get(s) != Some(&b'$') {
        return None;
    }
    let mut e = s + 1;
    while e < at && is_ident(b[e]) {
        e += 1;
    }
    if e == s + 1 {
        return None;
    }
    let eq = next_start(b, e);
    if b.get(eq) != Some(&b'=') || matches!(b.get(eq + 1), Some(b'=' | b'>')) {
        return None;
    }
    String::from_utf8(b[s + 1..e].to_vec()).ok()
}

/// A ternary arm: after a `?` (not `??`) or a `:`, before a `:` or `;`.
fn ternary_arm(b: &[u8], before: usize, after: usize) -> bool {
    let opens = (before > 0 && b[before - 1] == b':')
        || (before > 0 && b[before - 1] == b'?' && !(before > 1 && b[before - 2] == b'?'));
    opens && matches!(b.get(after), Some(b':' | b';'))
}

impl BodyScan {
    fn new(text: &str) -> BodyScan {
        let b = text.as_bytes();
        let mut occurrences = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'$' && b.get(i + 1).is_some_and(|&c| is_ident(c)) {
                let mut j = i + 1;
                while j < b.len() && is_ident(b[j]) {
                    j += 1;
                }
                if let Some(name) = text.get(i + 1..j) {
                    occurrences.push(Occurrence { name: name.to_owned(), start: i, end: j });
                }
                i = j;
            } else {
                i += 1;
            }
        }
        let mut edges: HashMap<String, Vec<String>> = HashMap::new();
        let mut copies = Vec::new();
        for o in &occurrences {
            let (before, after) = (prev_end(b, o.start), next_start(b, o.end));
            let plain = if b.get(after) == Some(&b';') { assign_target(b, before) } else { None };
            let lhs = plain.or_else(|| {
                if ternary_arm(b, before, after) { ternary_target(b, o.start) } else { None }
            });
            if let Some(lhs) = lhs {
                edges.entry(lhs.clone()).or_default().push(o.name.clone());
                edges.entry(o.name.clone()).or_default().push(lhs.clone());
                copies.push((lhs, o.name.clone(), o.start));
            }
        }
        let poisoned = POISON.iter().any(|p| text.contains(p));
        BodyScan {
            text: text.to_owned(),
            occurrences,
            edges,
            copies,
            poisoned,
            disallowed: RefCell::new(HashMap::new()),
        }
    }

    /// The disallowed occurrences (at most two) of `subject`'s copy group, memoized.
    fn disallowed_of(&self, subject: &str) -> Rc<Ranges> {
        if let Some(hit) = self.disallowed.borrow().get(subject) {
            return Rc::clone(hit);
        }
        let mut group: HashSet<&str> = HashSet::from([subject]);
        let mut work = vec![subject];
        while let Some(n) = work.pop() {
            for m in self.edges.get(n).into_iter().flatten() {
                if group.insert(m.as_str()) {
                    work.push(m.as_str());
                }
            }
        }
        let b = self.text.as_bytes();
        let mut out = Vec::new();
        for o in self.occurrences.iter().filter(|o| group.contains(o.name.as_str())) {
            if let Some(range) = self.classify(b, o, subject) {
                out.push(range);
                if out.len() == 2 {
                    break;
                }
            }
        }
        let out = Rc::new(out);
        self.disallowed.borrow_mut().insert(subject.to_owned(), Rc::clone(&out));
        out
    }

    /// `None` when the occurrence is one of the shapes (a), (c) or (d); otherwise the range of
    /// reported-use offsets for which it is the reported use itself (b).
    fn classify(&self, b: &[u8], o: &Occurrence, subject: &str) -> Option<(usize, usize)> {
        let (before, after) = (prev_end(b, o.start), next_start(b, o.end));
        // A write: `$v = ...` (not `==`, not `=>`).
        if b.get(after) == Some(&b'=') && !matches!(b.get(after + 1), Some(b'=' | b'>')) {
            return None;
        }
        // A plain copy source, or a ternary arm that is one.
        if b.get(after) == Some(&b';') && assign_target(b, before).is_some() {
            return None;
        }
        if ternary_arm(b, before, after) && ternary_target(b, o.start).is_some() {
            return None;
        }
        if preg_out_param(&self.text, o, b, before, after) {
            return None;
        }
        // A null test narrows the reported variable itself; on another member of the group it
        // counts only when that member is never a copy target and every copy of it comes after
        // the test (`if ($s !== null) { $y = $s; }`), never `$x2 = $x; if ($x2 !== null)`.
        if null_test(b, o, before, after)
            && (o.name == subject
                || self.copies.iter().all(|(lhs, src, at)| {
                    *lhs != o.name && (*src != o.name || *at > o.start)
                }))
        {
            return None;
        }
        // The reported use: the argument itself, or `return $v;` whose span may start at the
        // `return` keyword.
        let lo = if b.get(after) == Some(&b';') && word_ends_at(b, before, b"return") {
            before - "return".len()
        } else {
            o.start
        };
        Some((lo, o.start))
    }
}

/// The text before `end` allows a comparison to start an operand: the start of a condition or
/// a plain assignment's right-hand side, and not the operand of another operator.
fn operand_context(b: &[u8], end: usize) -> bool {
    if end == 0 {
        return true;
    }
    if b[end - 1] == b'(' {
        // A parenthesis opens a condition (`if (`, `while (`), a group or a negated group,
        // never a call's argument list.
        let c = prev_end(b, end - 1);
        return c == 0
            || word_ends_at(b, c, b"if")
            || word_ends_at(b, c, b"elseif")
            || word_ends_at(b, c, b"while")
            || b[c - 1] == b'!'
            || (b[c - 1] == b'(' && operand_context(b, c))
            || matches!(b[c - 1], b';' | b'{' | b'}' | b',' | b'?' | b':')
            || ends_with_at(b, c, b"&&")
            || ends_with_at(b, c, b"||")
            || word_ends_at(b, c, b"return")
            || word_ends_at(b, c, b"and")
            || word_ends_at(b, c, b"or")
            || assign_target(b, c).is_some();
    }
    matches!(b[end - 1], b';' | b'{' | b'}' | b',')
        || ends_with_at(b, end, b"&&")
        || ends_with_at(b, end, b"||")
        || word_ends_at(b, end, b"return")
        || word_ends_at(b, end, b"and")
        || word_ends_at(b, end, b"or")
        || assign_target(b, end).is_some()
}

/// What may follow a complete comparison that is a whole operand.
fn operand_follow(b: &[u8], at: usize) -> bool {
    let connective = |at: usize| {
        matches!(b.get(at), Some(b'?' | b';' | b',' | b'{'))
            || starts_with_at(b, at, b"&&")
            || starts_with_at(b, at, b"||")
            || word_starts_at(b, at, b"and")
            || word_starts_at(b, at, b"or")
    };
    if b.get(at) == Some(&b')') {
        let n = next_start(b, at + 1);
        return n >= b.len() || connective(n) || b[n] == b')';
    }
    connective(at)
}

/// (c) `$v === null`, `$v !== null`, `null === $v`, `null !== $v` with `null` a standalone
/// keyword and the comparison the whole operand.
fn null_test(b: &[u8], o: &Occurrence, before: usize, after: usize) -> bool {
    if (starts_with_at(b, after, b"===") || starts_with_at(b, after, b"!=="))
        && word_starts_at(b, next_start(b, after + 3), b"null")
    {
        let end = next_start(b, next_start(b, after + 3) + 4);
        return operand_context(b, before) && operand_follow(b, end);
    }
    if (ends_with_at(b, before, b"===") || ends_with_at(b, before, b"!=="))
        && word_ends_at(b, prev_end(b, before - 3), b"null")
    {
        let ctx = prev_end(b, prev_end(b, before - 3) - 4);
        return operand_context(b, ctx) && operand_follow(b, after) && o.end <= b.len();
    }
    false
}

/// (d) the occurrence is the bare third argument of a `preg_match` / `preg_match_all` call:
/// the out-parameter position. Found by parsing each such call's arguments forward, with
/// quotes and brackets skipped, so a comma inside a pattern never miscounts.
fn preg_out_param(text: &str, o: &Occurrence, b: &[u8], before: usize, after: usize) -> bool {
    if before == 0 || b[before - 1] != b',' || !matches!(b.get(after), Some(b')' | b',')) {
        return false;
    }
    for name in ["preg_match_all", "preg_match"] {
        let mut from = 0;
        while let Some(k) = text.get(from..).and_then(|t| t.find(name)) {
            let at = from + k;
            from = at + name.len();
            let open = next_start(b, from);
            if b.get(open) != Some(&b'(') || !word_ends_at(b, from, name.as_bytes()) {
                continue;
            }
            if third_argument_start(b, open + 1) == Some(o.start) {
                return true;
            }
        }
    }
    false
}

/// The byte offset of the first non-space character of the third argument of the call whose
/// arguments start at `at` (just after its `(`), or `None` when it has fewer than three.
fn third_argument_start(b: &[u8], at: usize) -> Option<usize> {
    let (mut depth, mut commas, mut i) = (0usize, 0usize, at);
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += usize::from(b[i] == b'\\') + 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return None,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                commas += 1;
                if commas == 2 {
                    return Some(next_start(b, i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::BodyScan;

    /// Every function and method body of every PHP file under the directory the environment
    /// variable `STEINS_GUARD_SCAN_CORPUS` names is scanned for every variable it mentions:
    /// no input may panic the scan. Skipped when the variable is unset.
    #[test]
    fn the_scan_never_panics_on_a_corpus() {
        let Some(root) = std::env::var_os("STEINS_GUARD_SCAN_CORPUS") else { return };
        let mut stack = vec![std::path::PathBuf::from(root)];
        let mut bodies = 0usize;
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for entry in rd.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "php")
                    && let Ok(src) = std::fs::read_to_string(&path)
                {
                    let tree = steins_syntax::SourceTree::parse(&src);
                    let methods = tree.classes().iter().flat_map(|c| c.methods.iter());
                    let spans = tree
                        .functions()
                        .iter()
                        .map(|f| f.body_span)
                        .chain(methods.filter_map(|m| m.body_span));
                    for span in spans {
                        let Some(text) = tree.source_slice(span) else { continue };
                        let scan = BodyScan::new(text);
                        let names: Vec<String> =
                            scan.occurrences.iter().map(|o| o.name.clone()).collect();
                        for n in names {
                            let _ = scan.disallowed_of(&n);
                        }
                        bodies += 1;
                    }
                }
            }
        }
        assert!(bodies > 0, "no body scanned");
    }
}
