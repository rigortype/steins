//! `return.compile-fatal` (ADR-0078 amendment 2026-10-11, issues #952, #1051): the
//! three `return` statements PHP's compiler refuses outright, judged from the
//! scope's written return hint and each `return`'s form alone.

use steins_syntax::{RetHint, RetHintKind, ReturnForm, ReturnSite, Scope, ScopeOwner};

use crate::cx::Cx;
use crate::project::Diagnostic;
use crate::RETURN_COMPILE_FATAL_ID;

// compile-fatal returns (ADR-0078 amendment 2026-10-11, issue #1051)

/// Every function-like body in this file, judged for [`RETURN_COMPILE_FATAL_ID`].
///
/// Env-free, folder-free and dead-region-free: the compiler rejects the file before any
/// code runs, so a `return` in `if (false) { … }` is as fatal as one at the top of the
/// body. The rule is per scope — a closure nested in a `: void` body is judged by its own
/// hint, never the enclosing one — and one finding is made per offending `return`.
///
/// # The rule table
///
/// Every row was run on PHP 8.5.11 (`php -l`). "silent" rows are either valid PHP or a
/// fatal this id does not own.
///
/// | written hint | `return` form | verdict |
/// | --- | --- | --- |
/// | `: void` | `return <expr>;`, `return null;` included | fire: "A void function must not return a value" |
/// | `: void` | `return;` | silent: legal |
/// | `: never` | `return;` or `return <expr>;` | fire: "A never-returning function must not return" |
/// | `: never` arrow `fn(): never => <expr>` | the body expression | silent: compiles (runtime error) |
/// | `: void` arrow `fn(): void => <expr>` | the body expression | fire: the void message |
/// | `: int`, `: ?int`, `: int\|string`, `: mixed`, `: array`, `: iterable`, `: static`, `: self`, `: Foo`, `: null`, `: false`, `: true`, … | `return;` | fire: "A function with return type must return a value" |
/// | the same hints | `return <expr>;`, `return null;` included | silent: compiles; `return null;` under `: int` is `type.return-mismatch`'s |
/// | `: ?void`, `: int\|void` | any | silent: a different fatal ("Void can only be used as a standalone type") |
/// | no written hint | any | silent |
/// | any hint, the scope contains `yield` | any | silent: a different fatal ("Generator return type must be a supertype of Generator") |
/// | constructor or destructor, no written hint | `return 5;` | silent: it compiles |
/// | constructor or destructor, a written hint (`__construct(): void`) | as in any method | fire: PHP fatals with the same sentence |
/// | a return inside a property hook nested in the body (plain, promoted or anonymous-class property) | any | belongs to the hook, never charged to the enclosing scope |
/// | abstract / interface method | none | silent: no body, no scope |
/// | a closure nested in the body | its own `return` | judged by the closure's own hint only |
/// | property hook body | any | silent: not judged (declined) |
/// | trait, enum and anonymous-class methods | any | silent: their bodies have no scope yet (#1051 R1) |
///
/// The wording PHP prints says "method" for a method and "function" for a function and a
/// closure (witnessed); the finding quotes the sentence for the scope's own kind.
pub(crate) fn check_compile_fatal_returns(cx: &Cx, out: &mut Vec<Diagnostic>) {
    for scope in cx.tree().scopes() {
        let Some(hint) = scope.ret_hint else { continue };
        if scope.is_generator || scope.returns.is_empty() {
            continue;
        }
        // Function-likes only: a hook's "return type" is the property's, and the hook
        // body is not a function-like PHP gives these rules to (declined).
        let noun = match &scope.owner {
            ScopeOwner::Function { .. } | ScopeOwner::Closure { .. } => "function",
            ScopeOwner::Method { .. }
            | ScopeOwner::TraitMethod { .. }
            | ScopeOwner::AnonMethod { .. } => "method",
            ScopeOwner::PropertyHook { .. } | ScopeOwner::TopLevel => continue,
        };
        if declares_void_inside_a_type(cx, hint) {
            continue;
        }
        for site in &scope.returns {
            if let Some(php_message) = fatal_for(hint.kind, site, noun) {
                out.push(finding(cx, scope, hint, site, &php_message));
            }
        }
    }
}

/// PHP's compile-time sentence for one `return` under one written hint, or `None` when
/// the pair compiles (or fatals with a sentence that is not this id's).
fn fatal_for(kind: RetHintKind, site: &ReturnSite, noun: &str) -> Option<String> {
    match (kind, site.form) {
        (RetHintKind::Void, ReturnForm::Valued | ReturnForm::ArrowBody) => {
            Some(format!("A void {noun} must not return a value"))
        }
        (RetHintKind::Void, ReturnForm::Bare) => None,
        // An arrow function's body is compiled with no `never` check at all.
        (RetHintKind::Never, ReturnForm::ArrowBody) => None,
        (RetHintKind::Never, ReturnForm::Bare | ReturnForm::Valued) => {
            Some(format!("A never-returning {noun} must not return"))
        }
        (_, ReturnForm::Bare) => {
            let article = if noun == "method" { "A method" } else { "A function" };
            Some(format!("{article} with return type must return a value"))
        }
        (_, ReturnForm::Valued | ReturnForm::ArrowBody) => None,
    }
}

/// `?void` and `int|void` are not `: void`: PHP stops on "Void can only be used as a
/// standalone type" first, so the hint is declined whatever the body returns. The
/// lowering classifies both as `Other`, which only the written text tells apart.
fn declares_void_inside_a_type(cx: &Cx, hint: RetHint) -> bool {
    hint.kind != RetHintKind::Void
        && cx.tree().text_at(hint.span).is_some_and(|text| {
            text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| word.eq_ignore_ascii_case("void"))
        })
}

fn finding(
    cx: &Cx,
    scope: &Scope,
    hint: RetHint,
    site: &ReturnSite,
    php_message: &str,
) -> Diagnostic {
    let declared = cx.tree().text_at(hint.span).unwrap_or("the declared type").trim();
    let subject = match &scope.owner {
        ScopeOwner::Function { name, .. } => format!("function {name}"),
        ScopeOwner::Method { class, method } | ScopeOwner::TraitMethod { class, method } => {
            format!("method {class}::{method}")
        }
        ScopeOwner::AnonMethod { method, .. } => format!("method class@anonymous::{method}"),
        _ => format!("the closure on line {}", closure_line(cx, scope)),
    };
    let pos = cx.tree().position(site.span.start);
    Diagnostic {
        id: RETURN_COMPILE_FATAL_ID,
        path: cx.path().to_owned(),
        line: pos.line,
        column: pos.column,
        message: format!(
            "{subject} declares a return type of {declared}, and PHP refuses this return at \
             compile time, so the file cannot load — \"{php_message}\""
        ),
        facet: None,
        fix: None,
    }
}

fn closure_line(cx: &Cx, scope: &Scope) -> u32 {
    match &scope.owner {
        ScopeOwner::Closure { def_offset } => cx.tree().position(*def_offset).line,
        _ => 0,
    }
}

// end compile-fatal returns (ADR-0078 amendment 2026-10-11, issue #1051)
