//! The pre-pass over a frame's body that finds the drop subjects: every variable some
//! plain write stores a `new` into, which writes release nothing, and, in a
//! constructor, which property writes initialize.

use std::collections::{BTreeMap, BTreeSet};

use mago_span::HasSpan;
use mago_syntax::cst::{Expression, Node, Variable};

use super::{DropSubject, DropSubjects, Made, creations, object_free, property};
use crate::{bytes_to_string, children, strip_dollar, to_span};

/// What a constructor body shows about its writes to `$this`'s own properties.
#[derive(Default)]
struct Init {
    /// Whether the frame is a constructor; nothing is collected otherwise.
    active: bool,
    /// Whether anything that could write a property of the object has run: any use of
    /// `$this` but a property fetch with a literal name, a `self::`, `static::` or
    /// `parent::` call, a closure that could capture it. Source order, so a write
    /// before it is still the first.
    tainted: bool,
    /// The properties already fetched or written by name.
    seen: BTreeSet<String>,
    /// The starts of the writes that are the first touch of their property.
    writes: Vec<u32>,
}

pub(super) struct Collect {
    subjects: BTreeMap<String, DropSubject>,
    /// Every by-value parameter, whatever its hint: it holds a value on entry, so no
    /// write to one is clean.
    params: Vec<String>,
    /// The by-reference parameters: a write to one is the caller's variable's, residue.
    by_ref: Vec<String>,
    /// Locals some earlier write may have stored an object into.
    held: BTreeSet<String>,
    /// The writes of each local that release nothing.
    clean: BTreeMap<String, Vec<u32>>,
    loops: u32,
    goto: bool,
    init: Init,
}

impl Collect {
    pub(super) fn new(constructor: bool) -> Self {
        Self {
            subjects: BTreeMap::new(),
            params: Vec::new(),
            by_ref: Vec::new(),
            held: BTreeSet::new(),
            clean: BTreeMap::new(),
            loops: 0,
            goto: false,
            init: Init { active: constructor, ..Init::default() },
        }
    }

    pub(super) fn add_param(&mut self, name: String, by_ref: bool) {
        if by_ref { self.by_ref.push(name) } else { self.params.push(name) }
    }

    pub(super) fn add_subject(&mut self, name: String, subject: DropSubject) {
        self.subjects.insert(name, subject);
    }

    /// The subjects, with the clean writes each owns. A `goto` can run any write twice,
    /// so no write is clean in such a function.
    pub(super) fn finish(mut self) -> DropSubjects {
        let mut clean = std::mem::take(&mut self.clean);
        for (name, subject) in &mut self.subjects {
            if !self.goto {
                subject.clean = clean.remove(name).unwrap_or_default();
            }
        }
        self.subjects.retain(|_, s| !s.receivers.is_empty());
        let init_writes = if self.goto { Vec::new() } else { self.init.writes };
        DropSubjects { vars: self.subjects, init_writes }
    }

    pub(super) fn visit(&mut self, node: &Node<'_, '_>) {
        match node {
            // The right-hand side runs before the write.
            Node::Assignment(a) if a.operator.is_assign() => {
                self.visit(&Node::Expression(a.rhs));
                self.assign(a.lhs, a.rhs, a.span());
                self.init_write(a.lhs, a.span());
                self.visit(&Node::Expression(a.lhs));
                return;
            }
            // `static $x = new D;` is a write of `$x`, once per process.
            Node::StaticConcreteItem(item) => {
                self.visit(&Node::Expression(item.value));
                let name = strip_dollar(bytes_to_string(item.variable.name));
                self.assign_to(name, item.value, item.value.span());
                return;
            }
            Node::While(_) | Node::DoWhile(_) | Node::For(_) | Node::Foreach(_) => {
                self.loops += 1;
                children(node).iter().for_each(|c| self.visit(c));
                self.loops -= 1;
                return;
            }
            Node::Goto(_) | Node::Label(_) => self.goto = true,
            Node::PropertyAccess(p) if property::this_property(p.object, &p.property).is_some() => {
                self.init.seen.extend(property::this_property(p.object, &p.property));
                return;
            }
            Node::DirectVariable(dv) if strip_dollar(bytes_to_string(dv.name)) == "this" => {
                self.init.tainted = true;
            }
            Node::StaticMethodCall(c)
                if matches!(
                    c.class.unparenthesized(),
                    Expression::Self_(_) | Expression::Static(_) | Expression::Parent(_)
                ) =>
            {
                self.init.tainted = true;
            }
            // A closure may capture `$this` and run before a later write.
            Node::Closure(_) | Node::ArrowFunction(_) => {
                self.init.tainted = true;
                return;
            }
            // Nested scopes are their own frames; an anonymous class's constructor
            // arguments are this frame's.
            Node::AnonymousClass(ac) => {
                if let Some(list) = &ac.argument_list {
                    self.visit(&Node::PartialArgumentList(list));
                }
                return;
            }
            Node::Function(_)
            | Node::Class(_)
            | Node::Interface(_)
            | Node::Trait(_)
            | Node::Enum(_) => return,
            _ => {}
        }
        children(node).iter().for_each(|c| self.visit(c));
    }

    /// A plain assignment `lhs = rhs` to a variable.
    fn assign(&mut self, lhs: &Expression<'_>, rhs: &Expression<'_>, span: mago_span::Span) {
        let Expression::Variable(Variable::Direct(dv)) = lhs.unparenthesized() else { return };
        self.assign_to(strip_dollar(bytes_to_string(dv.name)), rhs, span);
    }

    /// A plain write of `rhs` to the variable `name`.
    fn assign_to(&mut self, name: String, rhs: &Expression<'_>, span: mago_span::Span) {
        if name == "this" || self.by_ref.contains(&name) {
            return;
        }
        let param = self.params.contains(&name);
        let start = to_span(span).start;
        // The write releases nothing when no earlier write could have stored an object.
        if !param && self.loops == 0 && !self.held.contains(&name) {
            self.clean.entry(name.clone()).or_default().push(start);
        }
        let mut made = Made::default();
        creations(rhs, true, &mut made);
        if made.any || !object_free(rhs) {
            self.held.insert(name.clone());
        }
        if !made.any {
            return;
        }
        let subject = self.subjects.entry(name).or_default();
        for receiver in made.receivers {
            if !subject.receivers.contains(&receiver) {
                subject.receivers.push(receiver);
            }
        }
    }

    /// A plain `$this->p = …` of a constructor body that nothing has touched `p` or
    /// the object before, outside a loop: the property is uninitialized, or holds its
    /// default, which the resolver reads off the declaration.
    fn init_write(&mut self, lhs: &Expression<'_>, span: mago_span::Span) {
        if !self.init.active {
            return;
        }
        let Some(name) = property::this_property_of(lhs) else { return };
        let first = self.init.seen.insert(name);
        if first && !self.init.tainted && self.loops == 0 {
            self.init.writes.push(to_span(span).start);
        }
    }
}
