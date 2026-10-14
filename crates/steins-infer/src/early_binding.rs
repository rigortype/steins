//! Whether a declaration has run where a lane judges a use of it: conditional
//! declarations (ADR-0049 A2i) and PHP's early binding (hoisting), in one place
//! (ADR-0049 A26, issue #1035).
//!
//! A lane that checks a call against a signature, or a member against a
//! hierarchy, rests on the declaration having run before the use. PHP binds a
//! declaration either at compile time (hoisted: it exists before the file's first
//! statement runs) or when execution reaches it, and a declaration under a
//! condition only if the condition held. When the declaration has not run, PHP
//! raises "Call to undefined function" or "Class not found" instead of the error
//! the lane names. Every lane that used to read `conditional && !dam.is_clear()`
//! asks this module instead.
//!
//! ## The rule table
//!
//! Every PHP column was run on PHP 8.5.11 (opcache off; the hoisting rows also
//! with `opcache.enable_cli=1`, same answers unless a row says otherwise).
//! "Above" means a use in the file's **top-level code** (no enclosing function,
//! method, closure, arrow function or property hook) at a byte offset before the
//! declaration's name, in the same file. Every other use is "below": a body runs
//! when it is called, which is after the top-level code above it has run (the
//! call itself is top-level code or another body); a use in another file sees
//! the declaration once the declaring file has run, and a top-level declaration
//! runs with its file.
//!
//! | # | declaration | use | PHP 8.5.11 | verdict |
//! |---|-------------|-----|------------|---------|
//! | 1 | top-level `function f` | anywhere | hoisted: the call runs (`f(); function f() {}`) | judge |
//! | 2 | `function f` under `if`, a loop or a body | below, or another file | `f` exists iff the block ran: the judged error, or "Call to undefined function" (`if (false) { function f(int $a) {} } f();`) | judge, append the disjunction |
//! | 3 | the same | above, same file | "Call to undefined function" (`f(); if (true) { function f() {} }`) | decline |
//! | 3b | the same | inside a function-like body above it | the body runs when called: `function g() { f(); } if (…) { function f(int $a) {} } g();` is ArgumentCountError | judge, append the disjunction |
//! | 4 | top-level `class X {}`, `abstract`/`final`/`readonly` alike, also in a braced namespace or a bare `{ }` | anywhere | hoisted (`new X(); class X {}`) | judge |
//! | 5 | top-level `class X extends B`, `B` hoisted and declared **earlier** in the file, or an engine class | anywhere | hoisted (`class B {} class X extends B {}`, `extends Exception`, `extends ArrayObject`) | judge |
//! | 6 | top-level `class X extends B`, `B` declared later, absent, in another file, conditional, or not itself hoisted | above, same file | "Class not found" (`new X(); class X extends B {} class B {}`; a parent in another file binds only if that file already ran) | decline |
//! | 7 | top-level class with any `implements`, any `use T`, an `enum`, an `interface` that `extends` (even `Countable`) | above, same file | "Class not found" (`new X(); interface I {} class X implements I {}`) | decline |
//! | 8 | rows 6 and 7 | below, inside a function-like body above it, or another file | the declaration ran before the use runs (`class F { function make() { return new X(1); } } interface I {} class X implements I { … } (new F)->make();` is ArgumentCountError) | judge |
//! | 9 | a class declared under `if` or a body, or one with such an ancestor, dam clear | below, or another file | the class exists iff the block ran: the judged error, or "Class not found" (`if (rand(0, 1)) { class K {} } (new K)->nope();`) | judge, append the disjunction |
//! | 10 | the same | above, same file | "Class not found" (`new X(); class X extends B {} if (true) { class B {} }`) | decline |
//! | 11 | a conditional declaration anywhere the claim rests on, dam standing | anywhere | which declaration binds is load order (A2i) | decline |
//! | 12 | top-level `interface I {}` / `trait T {}` with no parents or uses | anywhere | hoisted (`echo I::K; interface I { const K = 1; }`) | judge |
//!
//! Rows 1 to 11 are the verdicts of [`function_use`] and [`class_use`], which the
//! lanes that judge a **use that names the class or function** ask: arity
//! (`call.too-few-arguments`, `call.unknown-named-argument`),
//! `call.undefined-method`, `class-const.undefined`,
//! `call.inaccessible-method` and `class-const.inaccessible`. The disjunction
//! keeps the lane's id and says what PHP raises in the other world (decision D3
//! of #1051): both worlds fail, so the finding stands, but its message names
//! both errors.
//!
//! Lanes judging a **value** (a property read on an object, a declared receiver,
//! an object in string context) and the declaration fatals ask only
//! [`redammed`], row 11: an instance exists only once its class and every
//! ancestor ran, and a declaration's own fatal is raised when it runs. So do the
//! existence verdicts (`method_exists`, `function_exists`, the `defined('C::K')`
//! fold, a callable's absence), whose `Yes`/`No` answers keep their A2i leg as it
//! was.
//!
//! Not modelled: a class whose method signature names a class not yet loaded
//! (`?C` in a variance check against a parent) is bound late without opcache
//! ("Class not found" above it on PHP 8.5.11) and early with
//! `opcache.enable_cli=1`; row 5 judges it, as the lanes did before. A body
//! called from top-level code above the declaration it uses runs before that
//! declaration binds; row 8 judges it, as the lanes did before. And, each a
//! load-time fatal of its own that no lane names: a class
//! using a trait that is not yet declared ("Trait not found",
//! `class A { use T2; } trait T2 { use T3; } trait T3 {}`), a class whose parent
//! is not yet declared when it runs ("Class not found" at the declaration), and a
//! class constant naming an absent class (raised at the first `new`). A use
//! below such a declaration is never reached, so its finding names a line PHP
//! does not run, as for any earlier fatal.

use steins_syntax::{ClassDecl, FunctionDecl};

use crate::cx::Cx;
use crate::fold::Folder;
use crate::project::{Res, Site};

/// What a lane does with a claim resting on a declaration at one use site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Binding {
    /// The declaration has run wherever the use runs: judge as before.
    Judge,
    /// The declaration ran only if its condition held: judge, and append the
    /// disjunction naming the other world's error.
    Disjoin(Subject),
    /// The use may run before the declaration, or another declaration may bind:
    /// the lane stays silent.
    Decline,
}

/// What kind of name the disjunction is about, which decides PHP's other error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Subject {
    Function,
    Class,
}

impl Binding {
    /// The text a lane appends to its message: empty unless [`Self::Disjoin`].
    pub(crate) const fn suffix(self) -> &'static str {
        match self {
            Self::Judge | Self::Decline => "",
            Self::Disjoin(Subject::Function) => {
                "; or Error \"Call to undefined function\" if its conditional declaration has not run"
            }
            Self::Disjoin(Subject::Class) => {
                "; or Error \"Class not found\" if a conditional declaration in its hierarchy has not run"
            }
        }
    }
}

/// Row 11 (ADR-0049 A2i): a claim resting on a conditional declaration while a dam
/// site stands. Some other definition may bind, so which body PHP runs is load order.
pub(crate) fn redammed(cx: &Cx, any_conditional: bool) -> bool {
    any_conditional && !cx.dam.is_clear()
}

/// The verdict for a use of the function `site` at byte offset `at` of the current
/// file (rows 1 to 3, and 11).
pub(crate) fn function_use(cx: &Cx, site: Site, at: u32) -> Binding {
    let decl: &FunctionDecl = cx.fn_decl(site);
    if !decl.conditional {
        return Binding::Judge;
    }
    if redammed(cx, true) || (site.file == cx.cur && top_level_above(cx, at, decl.span.start)) {
        return Binding::Decline;
    }
    Binding::Disjoin(Subject::Function)
}

/// The verdict for a use of the class `class_fqn` at byte offset `at` of the current
/// file, the claim resting on a hierarchy whose conditional bit is `any_conditional`
/// (rows 4 to 11). Only the class the site names can be above its own declaration:
/// once it exists, every ancestor does.
pub(crate) fn class_use(
    cx: &Cx,
    folder: &mut dyn Folder,
    class_fqn: &str,
    any_conditional: bool,
    at: u32,
) -> Binding {
    if redammed(cx, any_conditional) {
        return Binding::Decline;
    }
    if let Some((file, cd)) = cx.find_class(class_fqn)
        && file == cx.cur
        && top_level_above(cx, at, cd.span.start)
        && !class_hoisted(cx, folder, file, cd)
    {
        return Binding::Decline;
    }
    if any_conditional { Binding::Disjoin(Subject::Class) } else { Binding::Judge }
}

/// Whether the use at `at` is top-level code of the current file before the
/// declaration at `decl` ("above" in the rule table). A use inside a function-like
/// body runs only when the body is called, after the top-level code above it.
fn top_level_above(cx: &Cx, at: u32, decl: u32) -> bool {
    at < decl && !cx.tree().in_function_like(at)
}

/// Whether PHP binds `cd` (declared in `file`) at compile time, before the file's
/// first statement runs (rows 4 to 7, and 12).
fn class_hoisted(cx: &Cx, folder: &mut dyn Folder, file: usize, cd: &ClassDecl) -> bool {
    if cd.conditional
        || cd.is_enum
        || cd.uses_traits
        || !cd.used_traits.is_empty()
        || !cd.implements.is_empty()
    {
        return false;
    }
    let Some(parent) = &cd.parent else {
        return true;
    };
    if cd.is_interface {
        return false; // an interface's `extends`, even of an engine interface.
    }
    let fqn = cx.units[file].tree.resolve_class_fqn(parent);
    match cx.index.resolve_class(&fqn) {
        // A project parent binds X early only when it is declared earlier in the same
        // file and is itself bound early; the name must be its own, not an alias.
        Res::Unique(site) => {
            let (pfile, pcd) = cx.class_decl(site);
            pfile == file
                && pcd.span.start < cd.span.start
                && pcd.fqn.eq_ignore_ascii_case(&fqn)
                && class_hoisted(cx, folder, pfile, pcd)
        }
        Res::Ambiguous => false,
        // An engine parent is resident before any script runs.
        Res::Absent => folder.boot_surface_class_like(&fqn) == Some(true),
    }
}
