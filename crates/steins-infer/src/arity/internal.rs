//! Arity of a call to an **internal** function or an **engine** class's method
//! (issue #1051 slice S5, from #937), judged against the analysing PHP's own
//! reflection ([`Folder::builtin_param_types`], [`Folder::reflected_class`]).
//!
//! One row per call shape (PHP 8.5.11 `php -r`-checked; "decline" is silence):
//!
//! ```text
//! call shape                                         verdict
//! strlen(), intdiv(1), str_replace("a","b"),         call.too-few-arguments
//!   max(), compact(), var_dump()
//! strlen("a","b"), array_keys with four,             call.too-many-arguments
//!   session_id(1,2)                                    (non-variadic targets only)
//! array_merge(), str_pad("a", 3), extras to a        silent
//!   variadic
//! strlen(str: "a")  (non-variadic, unknown name)     call.unknown-named-argument
//! sprintf(format: "x", foo: 1)  (variadic)           decline: call_user_func and kin
//!                                                      forward named extras
//! strlen("a", string: "b")  (name over a positional) decline (overwrite Error)
//! rand(1)                                            silent: reflection says 0 required,
//!                                                      a known false negative
//! f(...$xs), strlen(...)                             decline: count unproven / not a call
//! \strlen(), strlen() in a namespace                 judged: the global function runs
//! Ns\strlen(), a project function named strlen      decline: not the reflected function
//! no live engine, skewed PHP, monkey-patcher         decline (builtin_param_types is None)
//! $d->format(), DateTime::createFromFormat() on      call.too-few-arguments
//!   an exact engine class
//! new DateTimeZone()  (required constructor param)   call.too-few-arguments
//! new DateTime("now", null, 3)  (surplus)            silent: ReflectedMethod carries no
//!                                                      variadic flag (false negative)
//! abstract/interface/enum class, non-public or       decline
//!   abstract method, Class::nonStatic()
//! a project class, even one extending an engine one  decline: the project arm owns it
//! new DatePeriod(), gnupg_*()                        decline: hand-parsed / misreflected
//! ```
//!
//! The required count is one past the last non-optional parameter, which is
//! `getNumberOfRequiredParameters()` for an internal function (its `isOptional()`
//! is `position >= required`).

use steins_sidecar::{BuiltinParam, ReflectedClass, ReflectedClassKind, Visibility};
use steins_syntax::{CallExpr, NamedArg};

use super::{ArityReceiver, arity_method_receiver};
use crate::cx::Cx;
use crate::env::Store;
use crate::existence::global_function_callee;
use crate::fold::Folder;
use crate::project::Diagnostic;
use crate::version_region::in_version_dependent_region;
use crate::{
    CALL_TOO_FEW_ARGUMENTS_ID, CALL_TOO_MANY_ARGUMENTS_ID, CALL_UNKNOWN_NAMED_ARGUMENT_ID,
};

/// The extensions php-src ships (`getExtensionName()`, lowercased), whose arginfo is
/// the engine's contract. A PECL extension's arginfo varies with its version and can be
/// wrong (`gnupg`'s understates its arity, functions and methods alike), so a function,
/// class or method from any other extension is never judged.
const BUNDLED_EXTENSIONS: &[&str] = &[
    "core", "standard", "date", "spl", "pcre", "json", "ctype", "mbstring", "hash", "random",
    "reflection", "tokenizer", "filter", "iconv", "session", "posix", "pcntl", "sockets",
    "curl", "openssl", "zlib", "bcmath", "bz2", "calendar", "dom", "simplexml", "xml",
    "xmlreader", "xmlwriter", "libxml", "pdo", "pdo_mysql", "pdo_pgsql", "pdo_sqlite",
    "pdo_odbc", "pdo_dblib", "phar", "readline", "sodium", "sqlite3", "intl", "gd", "gettext",
    "gmp", "exif", "fileinfo", "ftp", "ldap", "mysqli", "mysqlnd", "odbc", "pgsql", "shmop",
    "snmp", "soap", "sysvmsg", "sysvsem", "sysvshm", "tidy", "xsl", "zip",
];

/// Whether an extension name is one php-src bundles. The list is the common set; a
/// bundled extension missing from it is silence, never a finding.
fn bundled(extension: Option<&str>) -> bool {
    extension.is_some_and(|e| BUNDLED_EXTENSIONS.iter().any(|b| e.eq_ignore_ascii_case(b)))
}

/// Bundled methods that parse their arguments by hand and throw something other than
/// `ArgumentCountError` for a short list, so the finding would name the wrong
/// consequence: `DatePeriod`'s overloaded constructor throws `TypeError`, and
/// `Phar::setStub()` / `PharData::setStub()` throw `UnexpectedValueException` on a
/// plain archive. (class, method), lowercased.
const HAND_PARSED_METHODS: [(&str, &str); 3] =
    [("dateperiod", "__construct"), ("phar", "setstub"), ("phardata", "setstub")];

fn hand_parsed(class: &str, method: &str) -> bool {
    HAND_PARSED_METHODS
        .iter()
        .any(|(c, m)| class.eq_ignore_ascii_case(c) && method.eq_ignore_ascii_case(m))
}

/// What the call's argument list proves about an internal signature.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// A named argument binding no parameter of a non-variadic function.
    UnknownNamed(String),
    /// A required parameter covered by nothing.
    TooFew { passed: usize, required: usize, missing: Option<(usize, String)> },
    /// More positional arguments than a non-variadic function declares.
    TooMany { passed: usize, accepted: usize },
}

/// The number of required parameters of a reflected list: one past the last
/// non-optional position.
pub(super) fn required_of(params: &[BuiltinParam]) -> usize {
    params.iter().rposition(|p| !p.optional).map_or(0, |i| i + 1)
}

/// Judge `pos` positional and `named` named arguments against `params`, in PHP's
/// precedence: overwrite (silent) over unknown name over too few over too many.
pub(super) fn judge(params: &[BuiltinParam], pos: usize, named: &[NamedArg]) -> Option<Verdict> {
    if named.iter().any(|n| params.iter().position(|p| p.name == n.name).is_some_and(|i| i < pos))
    {
        return None;
    }
    let variadic = params.iter().any(|p| p.variadic);
    if let Some(unknown) = named.iter().find(|n| !params.iter().any(|p| p.name == n.name)) {
        // A variadic internal function may forward named extras (`call_user_func`):
        // the verdict is the function's own, so decline.
        return (!variadic).then(|| Verdict::UnknownNamed(unknown.name.clone()));
    }
    let required = required_of(params);
    let uncovered =
        (0..required).any(|i| i >= pos && !named.iter().any(|n| n.name == params[i].name));
    if uncovered {
        // As PHP names it: the first required parameter nothing covers.
        let missing = (0..required)
            .find(|&i| i >= pos && !named.iter().any(|n| n.name == params[i].name))
            .filter(|_| !named.is_empty())
            .map(|i| (i + 1, params[i].name.clone()));
        return Some(Verdict::TooFew { passed: pos + named.len(), required, missing });
    }
    (named.is_empty() && !variadic && pos > params.len())
        .then_some(Verdict::TooMany { passed: pos, accepted: params.len() })
}

/// Whether the call's shape lets its argument count be read at all: no unproven
/// unpacking, and not the first-class-callable shape `f(...)`.
fn countable(call: &CallExpr) -> bool {
    !call.has_spread
        && (call.positional_only || !call.args.is_empty() || !call.named_args.is_empty())
}

fn emit(cx: &Cx, call: &CallExpr, display: &str, verdict: Verdict, out: &mut Vec<Diagnostic>) {
    // The signature is the analysing PHP's; a branch that runs on another PHP is
    // judged by a signature it never sees.
    if in_version_dependent_region(cx, call.span.start) {
        return;
    }
    let (id, message) = match verdict {
        Verdict::UnknownNamed(name) => (
            CALL_UNKNOWN_NAMED_ARGUMENT_ID,
            format!(
                "unknown named argument ${name} to {display}() — no parameter ${name}, provable Error",
            ),
        ),
        Verdict::TooFew { passed, required, missing } => {
            // With named arguments the count alone misleads (`implode(array: [1])` is
            // 1 passed of 1 required): name the parameter PHP reports as not passed.
            let named = missing
                .map(|(n, name)| format!("; argument #{n} (${name}) not passed"))
                .unwrap_or_default();
            (
                CALL_TOO_FEW_ARGUMENTS_ID,
                format!(
                    "too few arguments to {display}(): {passed} passed, {required} required{named} — provable ArgumentCountError",
                ),
            )
        }
        Verdict::TooMany { passed, accepted } => (
            CALL_TOO_MANY_ARGUMENTS_ID,
            format!(
                "too many arguments to {display}(): {passed} passed, at most {accepted} accepted — provable ArgumentCountError",
            ),
        ),
    };
    let at = cx.tree().position(call.span.start);
    out.push(Diagnostic {
        id,
        facet: None,
        fix: None,
        path: cx.path().to_owned(),
        line: at.line,
        column: at.column,
        message,
    });
}

/// The reflected signature of the global builtin a call names, or `None` where the
/// call does not denote one the analysing PHP reflects.
pub(super) fn builtin_signature(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
) -> Option<(String, Vec<BuiltinParam>)> {
    let name = global_function_callee(cx, call)?;
    // A project function of this simple name shadows the builtin or makes the name
    // ambiguous: what runs is not what the engine reflected.
    if cx.index.has_simple_function(name) {
        return None;
    }
    let params = folder.builtin_param_types(name)?;
    Some((name.to_owned(), params))
}

/// The internal-function arm: `strlen()`, `strlen("a", "b")`, `strlen(str: "a")`.
pub(super) fn check_function(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    out: &mut Vec<Diagnostic>,
) {
    if !countable(call) {
        return;
    }
    let Some((name, params)) = builtin_signature(cx, folder, call) else { return };
    if let Some(verdict) = judge(&params, call.args.len(), &call.named_args)
        && function_is_bundled(folder, &name)
    {
        emit(cx, call, &name, verdict, out);
    }
}

/// Whether the builtin belongs to an extension php-src bundles. Asked only once a
/// verdict is in hand.
pub(super) fn function_is_bundled(folder: &mut dyn Folder, name: &str) -> bool {
    bundled(folder.builtin_function_extension(name).as_deref())
}

/// The engine class a receiver names, when no project file declares it and the
/// analysing PHP has it as an internal, instantiable-kind class.
fn engine_class(cx: &Cx, folder: &mut dyn Folder, fqn: &str) -> Option<ReflectedClass> {
    if !cx.class_absent(fqn) || !folder.absence_family_available() {
        return None;
    }
    let decl = folder.reflected_class(fqn.trim_start_matches('\\'))?.declaration?;
    (decl.internal
        && bundled(decl.extension.as_deref())
        && decl.kind == ReflectedClassKind::Class
        && !decl.is_abstract)
        .then_some(decl)
}

/// The engine-method arm: too few arguments to a method or constructor of an
/// engine class the receiver provably is. Reflection carries counts and no names or
/// variadic flag, so only positional too-few is judged.
pub(super) fn check_engine_method(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    if !call.positional_only || call.has_spread {
        return;
    }
    let Some(ArityReceiver { class, method, is_static_call, exact }) =
        arity_method_receiver(cx, call, store, poisoned)
    else {
        return;
    };
    if !exact {
        return;
    }
    let Some(decl) = engine_class(cx, folder, &class) else { return };
    if hand_parsed(&decl.name, &method) {
        return;
    }
    let Some(m) = decl.methods.iter().find(|m| m.name.eq_ignore_ascii_case(&method)) else {
        return;
    };
    if m.is_abstract || m.visibility != Visibility::Public || (is_static_call && !m.is_static) {
        return;
    }
    let (passed, required) = (call.args.len(), m.params_required as usize);
    if passed < required {
        let display = format!("{}::{}", decl.name, m.name);
        emit(cx, call, &display, Verdict::TooFew { passed, required, missing: None }, out);
    }
}
