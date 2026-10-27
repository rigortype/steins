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
//! * (a) a plain copy source, or the target of a plain assignment (a write, not a read);
//! * (b) the reported argument or return itself;
//! * (c) an operand of an exact null test: `$v !== null`, `$v === null`, `null !== $v`,
//!   `null === $v`, `is_null($v)`, `isset($v)` or `$v ?? <literal>`, the shapes the engine
//!   models exactly;
//! * (d) the out-parameter position of `preg_match` / `preg_match_all`, the seed position.
//!
//! Any other occurrence (a condition, a call argument, a stored boolean, a by-reference
//! helper, a closure `use`, an interpolation, an `echo`) declines. Over-silencing a new
//! finding is acceptable; the rule is only read when a row is about to report, so it costs
//! nothing otherwise. A function whose body cannot be found (a closure, the top level)
//! declines.

use std::collections::{HashMap, HashSet};

use steins_syntax::{ArgValue, Span};

use crate::cx::Cx;

/// One `$name` in the body: its name and the byte range of the `$name` token (body-relative).
struct Occurrence<'t> {
    name: &'t str,
    start: usize,
    end: usize,
}

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
    let Some(text) = cx.tree().source_slice(body) else { return true };
    let occurrences = occurrences(text);
    let group = copy_group(text, &occurrences, subject);
    let base = body.start as usize;
    occurrences
        .iter()
        .filter(|o| group.contains(o.name))
        .any(|o| !allowed(text, o, base, offset as usize))
}

/// The innermost function or method body span containing `offset`.
fn enclosing_body(cx: &Cx, offset: u32) -> Option<Span> {
    let tree = cx.tree();
    let inside = |s: &Span| s.start <= offset && offset < s.end;
    let functions = tree.functions().iter().map(|f| f.body_span);
    let methods = tree.classes().iter().flat_map(|c| c.methods.iter()).filter_map(|m| m.body_span);
    functions.chain(methods).filter(|s| inside(s)).min_by_key(|s| s.end - s.start)
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Every `$name` token of `text`, in order.
fn occurrences(text: &str) -> Vec<Occurrence<'_>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && is_ident(bytes[i + 1]) {
            let mut j = i + 1;
            while j < bytes.len() && is_ident(bytes[j]) {
                j += 1;
            }
            out.push(Occurrence { name: &text[i + 1..j], start: i, end: j });
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// The text before `o` with trailing whitespace removed, and the text after it with
/// leading whitespace removed.
fn around<'t>(text: &'t str, o: &Occurrence<'_>) -> (&'t str, &'t str) {
    (text[..o.start].trim_end(), text[o.end..].trim_start())
}

/// The `$lhs` of a plain assignment whose `=` ends `before` (`$lhs =`), if that is what
/// `before` ends with.
fn assign_target(before: &str) -> Option<&str> {
    let head = before.strip_suffix('=')?;
    // `==`, `!=`, `<=`, `>=`, `.=`, `+=`, `??=` and the like are not a plain assignment.
    if head.ends_with(['=', '!', '<', '>', '.', '+', '-', '*', '/', '%', '&', '|', '^', '?']) {
        return None;
    }
    let head = head.trim_end();
    let start = head.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let ident = &head[start + 1..];
    (head.as_bytes()[start] == b'$' && !ident.is_empty()).then_some(ident)
}

/// The variable a statement-level `$lhs = ... ? $v : ...;` assigns, for a ternary arm at
/// `o`: the statement starts after the previous `;`, `{` or `}`.
fn ternary_target<'t>(text: &'t str, o: &Occurrence<'_>) -> Option<&'t str> {
    let head = &text[..o.start];
    let from = head.rfind([';', '{', '}']).map_or(0, |p| p + 1);
    let stmt = text[from..o.start].trim_start();
    let rest = stmt.strip_prefix('$')?;
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let (ident, tail) = rest.split_at(end);
    let tail = tail.trim_start().strip_prefix('=')?;
    (!tail.starts_with(['=', '>']) && !ident.is_empty()).then_some(ident)
}

/// Whether `o` is a ternary arm: after a `?` (not `??`) or a `:`, before a `:` or `;`.
fn ternary_arm(before: &str, after: &str) -> bool {
    let opens = (before.ends_with('?') && !before.ends_with("??")) || before.ends_with(':');
    opens && (after.starts_with(':') || after.starts_with(';'))
}

/// The plain copy edges of the body, and the closure of `subject` over them.
fn copy_group<'t>(
    text: &'t str,
    occurrences: &[Occurrence<'t>],
    subject: &'t str,
) -> HashSet<&'t str> {
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut link = |a: &'t str, b: &'t str| {
        edges.entry(a).or_default().push(b);
        edges.entry(b).or_default().push(a);
    };
    for o in occurrences {
        let (before, after) = around(text, o);
        if after.starts_with(';')
            && let Some(lhs) = assign_target(before)
        {
            link(lhs, o.name);
        } else if ternary_arm(before, after)
            && let Some(lhs) = ternary_target(text, o)
        {
            link(lhs, o.name);
        }
    }
    let mut group = HashSet::from([subject]);
    let mut work = vec![subject];
    while let Some(n) = work.pop() {
        for &m in edges.get(n).into_iter().flatten() {
            if group.insert(m) {
                work.push(m);
            }
        }
    }
    group
}

/// Whether the occurrence `o` is one of the shapes (a)-(d) of the module header, or the
/// reported use itself at absolute byte `offset` (`base` is the body's start).
fn allowed(text: &str, o: &Occurrence<'_>, base: usize, offset: usize) -> bool {
    let abs = base + o.start;
    if abs == offset {
        return true;
    }
    let (before, after) = around(text, o);
    // A `return $v;` whose span starts at the `return` keyword.
    if after.starts_with(';')
        && let Some(kw) = before.strip_suffix("return")
        && !kw.ends_with(|c: char| is_ident(c as u8))
        && offset >= base + before.len() - "return".len()
        && offset <= abs
    {
        return true;
    }
    // (a) a write: `$v = ...` (not `==`, not `=>`).
    if let Some(rest) = after.strip_prefix('=')
        && !rest.starts_with(['=', '>'])
    {
        return true;
    }
    // (a) a plain copy source or a ternary arm that is one.
    if after.starts_with(';') && assign_target(before).is_some() {
        return true;
    }
    if ternary_arm(before, after) && ternary_target(text, o).is_some() {
        return true;
    }
    null_test(before, after) || preg_out_param(text, o, before, after)
}

/// (c) an exact null test with the occurrence as its operand.
fn null_test(before: &str, after: &str) -> bool {
    let is_null_word = |s: &str| {
        s.get(..4).is_some_and(|w| w.eq_ignore_ascii_case("null"))
            && !s[4..].starts_with(|c: char| is_ident(c as u8))
    };
    for op in ["!==", "==="] {
        if let Some(rest) = after.strip_prefix(op)
            && is_null_word(rest.trim_start())
        {
            return true;
        }
        if let Some(head) = before.strip_suffix(op)
            && head.trim_end().len() >= 4
            && head.trim_end()[head.trim_end().len() - 4..].eq_ignore_ascii_case("null")
        {
            return true;
        }
    }
    if after.starts_with(')') && (ends_call(before, "is_null") || ends_call(before, "isset")) {
        return true;
    }
    if let Some(rest) = after.strip_prefix("??") {
        let rest = rest.trim_start();
        return rest.starts_with(['\'', '"', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9'])
            || is_null_word(rest)
            || rest.starts_with("true")
            || rest.starts_with("false")
            || rest.starts_with("[]");
    }
    false
}

/// Whether `before` ends with `<name>(` for the function `name` (not a longer identifier).
fn ends_call(before: &str, name: &str) -> bool {
    let Some(head) = before.strip_suffix('(') else { return false };
    let head = head.trim_end();
    let Some(prefix) = head.strip_suffix(name) else { return false };
    !prefix.ends_with(|c: char| is_ident(c as u8) || c == '$' || c == '>' || c == ':')
}

/// (d) the occurrence is the bare third argument of a `preg_match` / `preg_match_all` call:
/// the out-parameter position. Found by parsing each such call's arguments forward, with
/// quotes and brackets skipped, so a comma inside a pattern never miscounts.
fn preg_out_param(text: &str, o: &Occurrence<'_>, before: &str, after: &str) -> bool {
    if !before.ends_with(',') || !(after.starts_with(')') || after.starts_with(',')) {
        return false;
    }
    for name in ["preg_match_all", "preg_match"] {
        let mut from = 0;
        while let Some(k) = text[from..].find(name) {
            let at = from + k;
            from = at + name.len();
            let rest = text[from..].trim_start();
            if !rest.starts_with('(') || !ends_with_name(&text[..from], name) {
                continue;
            }
            let args_at = text.len() - rest.len() + 1;
            if third_argument_start(text, args_at) == Some(o.start) {
                return true;
            }
        }
    }
    false
}

/// The byte offset of the first non-space character of the third argument of the call whose
/// arguments start at `at` (just after its `(`), or `None` when it has fewer than three.
fn third_argument_start(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let (mut depth, mut commas, mut i) = (0usize, 0usize, at);
    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    i += usize::from(bytes[i] == b'\\') + 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return None,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                commas += 1;
                if commas == 2 {
                    let tail = &text[i + 1..];
                    return Some(i + 1 + tail.len() - tail.trim_start().len());
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn ends_with_name(head: &str, name: &str) -> bool {
    head.strip_suffix(name)
        .is_some_and(|p| {
            !p.ends_with(|c: char| is_ident(c as u8) || c == '$' || c == '>' || c == ':')
        })
}
