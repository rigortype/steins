//! Whether a constant may hold an object (issue #868): the readings of [`ArgShape::GlobalConst`]
//! and [`ArgShape::ClassConst`].
//!
//! A constant is an object only where a declaration says so, and PHP lets two of them: a global
//! `const GO = new S;` (or a runtime `define()`), and, through it, a class constant that names it
//! (`class K { const X = GO; }`), however long the chain (`const Z = K::X;`). A class constant
//! cannot be `new Foo`; the only other object it holds is an enum case, which cannot declare
//! `__toString`. So the ToString family reads a class constant as holding no conversion exactly
//! when its initializer, and every constant the initializer names, ends in a scalar, an array of
//! them, or an enum case. A name the scan cannot follow (a class no file declares, a trait that
//! may supply it, `static::` in a class a subclass can extend, a global constant nothing states
//! the value of) is unproven.
//!
//! A project-declared global constant is read at its literal, but a `define($name, …)` with a
//! computed name, run before the file is included, wins over a later `const` (which only warns,
//! witnessed on PHP 8.5.11): a project with one reads no project constant as a scalar. The
//! engine's own constants cannot be redefined, so they stand.
//!
//! [`ArgShape::GlobalConst`]: steins_syntax::ArgShape::GlobalConst
//! [`ArgShape::ClassConst`]: steins_syntax::ArgShape::ClassConst

use std::collections::HashSet;

use steins_syntax::{ConstInit, ConstRef, DynamismKind, NameRef, StaticClass};

use super::{Frame, Held, fact_held};
use crate::cx::Cx;
use crate::global_consts::{engine_defines, global_const_fact};

/// How far a chain of constants is followed before it is unproven.
const DEPTH: u8 = 8;

impl Frame<'_> {
    /// What the global constant `name` holds: the value ADR-0094 reads it as, unless the project
    /// may have defined the name at run time.
    pub(super) fn global_const_held(&self, cx: &Cx, name: &NameRef) -> Held {
        let Some((fact, _)) = global_const_fact(cx, name) else { return Held::Unknown };
        let held = fact_held(&fact);
        if held == Held::ObjectFree && !engine_defines(cx, name) && self.computed_define(cx) {
            Held::Unknown
        } else {
            held
        }
    }

    /// Whether the project holds a `define()` whose name is computed, which may define any
    /// constant the engine does not.
    fn computed_define(&self, cx: &Cx) -> bool {
        *self.computed_define.get_or_init(|| {
            cx.units.iter().any(|unit| {
                unit.tree.dynamism_sites().iter().any(|s| s.kind == DynamismKind::DefineDynamic)
            })
        })
    }

    /// Whether the class constant `class::name`, written in this frame, holds no object a
    /// string conversion could run on: a scalar, an array of them or an enum case.
    pub(crate) fn class_const_no_object(&self, cx: &Cx, class: &StaticClass, name: &str) -> bool {
        self.class_const_free(cx, self.class_fqn, (class, name), 0)
    }

    fn class_const_free(
        &self,
        cx: &Cx,
        own: Option<&str>,
        (class, name): (&StaticClass, &str),
        depth: u8,
    ) -> bool {
        let start = match class {
            StaticClass::Named(r) => cx.class_fqn(r),
            StaticClass::SelfKw => match own {
                Some(own) => own.to_owned(),
                None => return false,
            },
            StaticClass::Parent => match own.and_then(|own| cx.parent_fqn(own)) {
                Some(parent) => parent,
                None => return false,
            },
            // The class the call was made on: a subclass may redeclare the constant.
            StaticClass::Static => match own {
                Some(own) if cx.class_has_no_subclass(own) => own.to_owned(),
                _ => return false,
            },
        };
        self.constant_free(cx, &start, name, depth)
    }

    /// The constant `name` of `start`'s hierarchy: its own declaration, an enum case, a parent's
    /// or an interface's, or an engine class's (every engine class constant is a scalar).
    fn constant_free(&self, cx: &Cx, start: &str, name: &str, depth: u8) -> bool {
        if depth > DEPTH {
            return false;
        }
        let mut pending = vec![start.to_owned()];
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(cur) = pending.pop() {
            if !seen.insert(cur.to_ascii_lowercase()) {
                continue;
            }
            let Some((file, cd)) = cx.find_class(&cur) else {
                if cx.class_absent(&cur) && steins_catalog::builtin_class_supers(&cur).is_some() {
                    return true;
                }
                return false;
            };
            if cd.is_enum && cd.enum_cases.iter().any(|c| c.name == name) {
                return true;
            }
            if let Some(decl) = cd.const_decls.iter().find(|d| d.name == name) {
                return self.init_free(&cx.at(file), &cd.fqn, &decl.init, depth + 1);
            }
            // A trait may supply the constant (PHP 8.2), and its body is not lowered.
            if cd.uses_traits {
                return false;
            }
            let tree = cx.units[file].tree;
            pending.extend(cd.parent.iter().map(|p| tree.resolve_class_fqn(p)));
            pending.extend(cd.implements.iter().map(|i| tree.resolve_class_fqn(i)));
        }
        // No class of a hierarchy the project reads declares it: an undefined constant raises.
        false
    }

    /// Whether every constant an initializer names ends in a scalar or an enum case.
    fn init_free(&self, cx: &Cx, owner: &str, init: &ConstInit, depth: u8) -> bool {
        !init.opaque
            && init.refs.iter().all(|r| match r {
                ConstRef::Class { class, name } => {
                    self.class_const_free(cx, Some(owner), (class, name), depth)
                }
                ConstRef::Global(name) => self.global_const_held(cx, name) == Held::ObjectFree,
            })
    }
}
