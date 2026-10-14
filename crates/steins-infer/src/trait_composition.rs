//! Trait composition (ADR-0105): the members a class or enum holds through its
//! `use` statements, flattened at query time through one per-universe memo.
//!
//! A trait's methods, properties and constants are lowered on the trait's own
//! [`ClassDecl`]; nothing is copied into the classes that import it. A consumer that
//! asks what a class-like *holds* asks [`Cx::held_method`], [`Cx::held_methods`],
//! [`Cx::held_property`] or [`Cx::held_const`], which read the class-like's own
//! members first and then its composition, and answer [`Open`] wherever the rule
//! table below declines. Walking the `extends` chain stays the consumer's: what a
//! class-like holds outranks what it inherits.
//!
//! # The composition rule table
//!
//! Every row witnessed on PHP 8.5.11 (`php` on the analysing machine). "Decline" is
//! an [`Open`] answer: the consumer treats the class-like as it treated any trait
//! user before this module, as unenumerable.
//!
//! | row | shape | PHP | here |
//! | --- | --- | --- | --- |
//! | C1 | the importer declares a member a trait also declares | the importer's wins | own member first; the trait's is not imported |
//! | C2 | a trait method and an inherited method of one name | the trait's wins (`T::m` over `P::m`) | the consumer reads held members before the parent |
//! | C3 | two traits give one method name from different declarations, both concrete, and the importer declares none | fatal "Trait method B::m has not been applied as C::m, because of collision with A::m" | decline the method table |
//! | C3a | the same, one abstract and one concrete | loads, the concrete one runs | the concrete one is held |
//! | C3b | the same, both abstract | loads (an abstract importer) | the first is held |
//! | C4 | one trait declaration reached twice (`use T, T`, or a diamond of `use`s) | loads, one member | one member; not a collision |
//! | C5 | an `insteadof` or `as` adaptation on any `use` in the composition | renames, excludes or re-scopes a member | decline every table (v1) |
//! | C6 | a trait that uses traits | its own members shadow what it imports, by the same rows | recursive, with a cycle refused |
//! | C7 | a used name that is no unique, unconditional trait declaration (absent, declared twice, under a condition, a class or an interface) | "Trait not found", or whichever declaration binds at run time | decline every table |
//! | C8 | an abstract trait method nothing implements | fatal "Class U contains 1 abstract method … (U::p)", naming the importer | held as an abstract method of the importer; the declaration-fatal pass names `Importer::m` |
//! | C8a | an abstract trait method an inherited concrete method implements | loads | the declaration-fatal pass discharges it from the chain |
//! | C9 | a concrete trait method over a `final` parent method (also a `private` trait method) | fatal "Cannot override final method Base::foo()" | the overriding family judges it as the importer's own method, naming the parent |
//! | C9a | an abstract trait method over a `final` parent method | loads | never judged as an override |
//! | C10 | a static and an instance member | composed alike | no distinction |
//! | C11 | a property the importer and a trait declare, or two traits from different declarations, compatible or not | compatible loads, else fatal "… define the same property" | decline the property table |
//! | C11a | a trait declares a hooked property | a hook runs user code | decline the property table |
//! | C12 | a constant the importer and a trait declare, or two traits (PHP 8.2) | compatible loads, else fatal "… define the same constant" | decline the constant table |
//!
//! A declaration in an unparsable file may have lost members to recovery
//! ([`Cx::member_incomplete`]), so a composition reaching one declines at query time:
//! the dam is a per-run answer and the memo is not.
//!
//! The memo is per universe and lazy: a composition is computed the first time a
//! consumer asks about its class-like, so a warm run decodes only the trees the
//! asking files reach (issue #516). A trait's whole member set is computed once and
//! shared by every importer, iteratively, so a deep `use` graph costs neither stack
//! nor repeated work.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use steins_syntax::{ClassDecl, MethodDecl, NameRef, PropertyDecl, Visibility};

use crate::cx::Cx;
use crate::project::{FileUnit, Index, Res};

/// Why a composition table declines (the rule rows above).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Open {
    /// C5: an `insteadof` or `as` adaptation.
    Adapted,
    /// C7: a used name that is no unique, unconditional trait declaration.
    Unresolved,
    /// C6: a trait that, through its own `use`s, uses itself.
    Cycle,
    /// C3, C11, C11a, C12: two declarations of one name that do not compose to one.
    Collision,
    /// A reached declaration lives in an unparsable file.
    Incomplete,
}

/// One member a trait declares: the trait's site and the member's index in its list
/// ([`ClassDecl::methods`], [`ClassDecl::properties`] or
/// [`ClassDecl::const_visibility`]). Two entries are the same member exactly when
/// they are equal, which is C4's "one declaration reached twice".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Member {
    file: u32,
    decl: u32,
    index: u32,
}

/// A member under its interned name.
#[derive(Debug, Clone, Copy)]
struct Entry {
    name: u32,
    member: Member,
}

/// A trait's whole member set, own and imported: what an importer of it receives.
#[derive(Debug)]
struct TraitFull {
    methods: Result<Vec<Entry>, Open>,
    properties: Result<Vec<Entry>, Open>,
    consts: Result<Vec<Entry>, Open>,
    /// Every trait declaration the set reaches, this one included.
    reached: Vec<Member>,
}

/// One imported member of a composition: what it is and which `use` brought it.
#[derive(Debug, Clone)]
struct Imported {
    /// The member's name: lowercase for a method, as written otherwise.
    name: Box<str>,
    member: Member,
    /// The index of the importer's [`ClassDecl::used_traits`] entry it came through.
    via: usize,
}

/// What a class or enum holds through its traits, apart from its own members.
#[derive(Debug)]
pub(crate) struct Composition {
    methods: Result<Vec<Imported>, Open>,
    properties: Result<Vec<Imported>, Open>,
    consts: Result<Vec<Imported>, Open>,
    /// The trait declarations reached, as sites.
    reached: Vec<(usize, usize)>,
}

/// Which of a [`Composition`]'s three tables a query reads.
type TablePick = fn(&Composition) -> &Result<Vec<Imported>, Open>;

/// The per-universe memo (ADR-0105). Carries no fact of its own, so two indexes
/// compare equal whatever either has computed.
#[derive(Default)]
pub(crate) struct TraitMemo(Mutex<MemoState>);

impl PartialEq for TraitMemo {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[derive(Default)]
struct MemoState {
    names: HashMap<Box<str>, u32>,
    spelled: Vec<Box<str>>,
    fulls: HashMap<Member, Arc<TraitFull>>,
    compositions: HashMap<(usize, u32), Arc<Composition>>,
}

/// The three member tables a composition reads.
#[derive(Clone, Copy)]
enum Table {
    Methods,
    Properties,
    Consts,
}

impl MemoState {
    fn intern(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.names.get(name) {
            return id;
        }
        let id = u32::try_from(self.spelled.len()).expect("fewer than 2^32 member names");
        self.names.insert(name.into(), id);
        self.spelled.push(name.into());
        id
    }
}

fn site_of(m: Member) -> (usize, usize) {
    (m.file as usize, m.decl as usize)
}

fn decl_at<'a>(units: &'a [FileUnit<'a>], m: Member) -> &'a ClassDecl {
    &units[m.file as usize].tree.classes()[m.decl as usize]
}

/// The name a table keys a member by: a method case-insensitively, a property and a
/// constant as written (PHP's own rule for each).
fn key_name(cd: &ClassDecl, table: Table, index: usize) -> String {
    match table {
        Table::Methods => cd.methods[index].name.to_ascii_lowercase(),
        Table::Properties => cd.properties[index].name.clone(),
        Table::Consts => cd.const_visibility[index].0.clone(),
    }
}

/// The importer's own names in one table, keyed as [`key_name`] keys them.
fn own_names(cd: &ClassDecl, table: Table) -> HashSet<String> {
    let len = match table {
        Table::Methods => cd.methods.len(),
        Table::Properties => cd.properties.len(),
        Table::Consts => cd.const_visibility.len(),
    };
    let mut out: HashSet<String> = (0..len).map(|i| key_name(cd, table, i)).collect();
    if let Table::Properties = table {
        out.extend(cd.hooked_properties.iter().cloned());
    }
    out
}

/// Resolve one `use`d name in its importer's file to a trait declaration (C7).
fn resolve_trait(units: &[FileUnit], index: &Index, file: usize, r: &NameRef) -> Option<Member> {
    let fqn = units[file].tree.resolve_class_fqn(r);
    let Res::Unique(site) = index.resolve_class(fqn.trim_start_matches('\\')) else { return None };
    let td = &units[site.file].tree.classes()[site.index];
    if !td.is_trait || td.conditional {
        return None;
    }
    Some(Member {
        file: u32::try_from(site.file).ok()?,
        decl: u32::try_from(site.index).ok()?,
        index: 0,
    })
}

/// Whether a trait declares a hooked property (C11a): a hook is user code on every
/// access, and the property table cannot say which.
fn hooks_a_property(cd: &ClassDecl) -> bool {
    !cd.hooked_properties.is_empty() || cd.properties.iter().any(|p| p.hooked)
}

/// Merge what the sources give one table of an importer, under rows C1, C3, C3a,
/// C3b, C4 and C11/C12. `sources` pairs each `use` index with the trait's table.
fn merge(
    units: &[FileUnit],
    state: &MemoState,
    own: &HashSet<String>,
    table: Table,
    sources: &[(usize, &Result<Vec<Entry>, Open>)],
) -> Result<Vec<(Entry, usize)>, Open> {
    let mut out: Vec<(Entry, usize)> = Vec::new();
    let mut at: HashMap<u32, usize> = HashMap::new();
    for (via, source) in sources {
        let entries = source.as_ref().map_err(|open| *open)?;
        for e in entries {
            if own.contains(&*state.spelled[e.name as usize]) {
                continue; // C1
            }
            let Some(&slot) = at.get(&e.name) else {
                at.insert(e.name, out.len());
                out.push((*e, *via));
                continue;
            };
            let prev = out[slot].0.member;
            if prev == e.member {
                continue; // C4
            }
            let Table::Methods = table else { return Err(Open::Collision) }; // C11, C12
            let abstract_at =
                |m: Member| decl_at(units, m).methods[m.index as usize].is_abstract;
            if abstract_at(e.member) {
                continue; // C3a, C3b: an abstract one never displaces
            }
            if !abstract_at(prev) {
                return Err(Open::Collision); // C3
            }
            out[slot] = (*e, *via); // C3a: the concrete one displaces an abstract one
        }
    }
    Ok(out)
}

/// The own entries of one table of a trait declaration.
fn own_entries(state: &mut MemoState, cd: &ClassDecl, at: Member, table: Table) -> Vec<Entry> {
    let len = match table {
        Table::Methods => cd.methods.len(),
        Table::Properties => cd.properties.len(),
        Table::Consts => cd.const_visibility.len(),
    };
    (0..len)
        .map(|i| Entry {
            name: state.intern(&key_name(cd, table, i)),
            member: Member { index: u32::try_from(i).unwrap_or(u32::MAX), ..at },
        })
        .collect()
}

/// Compute a trait's whole member set once its used traits' sets are in the memo
/// (C6): its own members, then what it imports.
fn full_of(units: &[FileUnit], index: &Index, state: &mut MemoState, at: Member) -> TraitFull {
    let cd = decl_at(units, at);
    let mut deps: Vec<(usize, Arc<TraitFull>)> = Vec::new();
    let mut failed: Option<Open> = cd.trait_adapted.then_some(Open::Adapted);
    for (via, r) in cd.used_traits.iter().enumerate() {
        match resolve_trait(units, index, at.file as usize, r) {
            None => failed = failed.or(Some(Open::Unresolved)),
            // A dependency still missing here is one in progress below this one: a cycle.
            Some(dep) => match state.fulls.get(&dep) {
                Some(full) => deps.push((via, Arc::clone(full))),
                None => failed = failed.or(Some(Open::Cycle)),
            },
        }
    }
    let mut reached = vec![at];
    for (_, d) in &deps {
        for r in &d.reached {
            if !reached.contains(r) {
                reached.push(*r);
            }
        }
    }
    if let Some(open) = failed {
        return TraitFull { methods: Err(open), properties: Err(open), consts: Err(open), reached };
    }
    let table_of = |state: &mut MemoState, table: Table| -> Result<Vec<Entry>, Open> {
        if let Table::Properties = table
            && hooks_a_property(cd)
        {
            return Err(Open::Collision);
        }
        let own = own_entries(state, cd, at, table);
        let own_set = own_names(cd, table);
        let sources: Vec<(usize, &Result<Vec<Entry>, Open>)> = deps
            .iter()
            .map(|(via, d)| {
                let t = match table {
                    Table::Methods => &d.methods,
                    Table::Properties => &d.properties,
                    Table::Consts => &d.consts,
                };
                (*via, t)
            })
            .collect();
        let mut merged = merge(units, state, &own_set, table, &sources)?;
        let mut out = own;
        out.extend(merged.drain(..).map(|(e, _)| e));
        Ok(out)
    };
    let methods = table_of(state, Table::Methods);
    let properties = table_of(state, Table::Properties);
    let consts = table_of(state, Table::Consts);
    TraitFull { methods, properties, consts, reached }
}

/// Fill the memo with `root`'s whole member set and every trait it reaches, in
/// post-order, without recursion (a `use` graph thousands deep is legal PHP).
fn ensure_full(units: &[FileUnit], index: &Index, state: &mut MemoState, root: Member) {
    let mut stack: Vec<(Member, bool)> = vec![(root, false)];
    let mut in_progress: HashSet<Member> = HashSet::new();
    while let Some((at, exit)) = stack.pop() {
        if state.fulls.contains_key(&at) {
            continue;
        }
        if exit {
            let full = full_of(units, index, state, at);
            state.fulls.insert(at, Arc::new(full));
            in_progress.remove(&at);
            continue;
        }
        if !in_progress.insert(at) {
            continue; // reached again from below itself: a cycle, refused at its exit.
        }
        stack.push((at, true));
        let cd = decl_at(units, at);
        for r in &cd.used_traits {
            if let Some(dep) = resolve_trait(units, index, at.file as usize, r)
                && !state.fulls.contains_key(&dep)
                && !in_progress.contains(&dep)
            {
                stack.push((dep, false));
            }
        }
    }
}

/// Compose an importing class or enum (every row at the importer's level).
fn compose(
    units: &[FileUnit],
    index: &Index,
    state: &mut MemoState,
    file: usize,
    cd: &ClassDecl,
) -> Composition {
    let mut deps: Vec<(usize, Arc<TraitFull>)> = Vec::new();
    let mut failed: Option<Open> = cd.trait_adapted.then_some(Open::Adapted);
    for (via, r) in cd.used_traits.iter().enumerate() {
        let Some(at) = resolve_trait(units, index, file, r) else {
            failed = failed.or(Some(Open::Unresolved));
            continue;
        };
        ensure_full(units, index, state, at);
        deps.push((via, Arc::clone(&state.fulls[&at])));
    }
    let mut reached: Vec<(usize, usize)> = Vec::new();
    for (_, d) in &deps {
        for r in &d.reached {
            if !reached.contains(&site_of(*r)) {
                reached.push(site_of(*r));
            }
        }
    }
    if let Some(open) = failed {
        return Composition { methods: Err(open), properties: Err(open), consts: Err(open), reached };
    }
    let table_of = |state: &MemoState, table: Table| -> Result<Vec<Imported>, Open> {
        let sources: Vec<(usize, &Result<Vec<Entry>, Open>)> = deps
            .iter()
            .map(|(via, d)| {
                let t = match table {
                    Table::Methods => &d.methods,
                    Table::Properties => &d.properties,
                    Table::Consts => &d.consts,
                };
                (*via, t)
            })
            .collect();
        let merged = merge(units, state, &own_names(cd, table), table, &sources)?;
        let mut out: Vec<Imported> = merged
            .into_iter()
            .map(|(e, via)| Imported {
                name: state.spelled[e.name as usize].clone(),
                member: e.member,
                via,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    };
    Composition {
        methods: table_of(state, Table::Methods),
        properties: table_of(state, Table::Properties),
        consts: table_of(state, Table::Consts),
        reached,
    }
}

impl TraitMemo {
    /// The composition of the class or enum `cd`, declared in `file`.
    fn composition(&self, units: &[FileUnit], index: &Index, file: usize, cd: &ClassDecl) -> Arc<Composition> {
        let key = (file, cd.span.start);
        let mut state = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(c) = state.compositions.get(&key) {
            return Arc::clone(c);
        }
        let c = Arc::new(compose(units, index, &mut state, file, cd));
        state.compositions.insert(key, Arc::clone(&c));
        c
    }
}

/// A method a class-like holds: declared by itself, or imported from a trait.
#[derive(Clone, Copy)]
pub(crate) struct HeldMethod<'a> {
    pub(crate) decl: &'a MethodDecl,
    /// The class-like that declares it: the holder itself, or the trait.
    pub(crate) owner: &'a ClassDecl,
    /// The file of [`Self::owner`].
    pub(crate) file: usize,
    /// For an imported method, the holder's `use` name it came through.
    pub(crate) via: Option<&'a NameRef>,
}

impl HeldMethod<'_> {
    /// Whether the method was imported from a trait (rows C2, C8, C9).
    pub(crate) fn imported(&self) -> bool {
        self.via.is_some()
    }
}

impl<'a> Cx<'a> {
    /// The composition of `cd` (declared in `file`), or `None` when it imports no
    /// trait. A trait's own `use`s are read through its importers, never here.
    fn composition_of(&self, file: usize, cd: &ClassDecl) -> Option<Arc<Composition>> {
        if cd.used_traits.is_empty() || cd.is_trait {
            return None;
        }
        Some(self.index.trait_memo().composition(self.units, self.index, file, cd))
    }

    /// The composition's answer for one table, with the per-run incompleteness leg.
    fn table<'c>(
        &self,
        comp: &'c Composition,
        pick: TablePick,
    ) -> Result<&'c [Imported], Open> {
        if comp.reached.iter().any(|&(file, _)| self.member_incomplete(file)) {
            return Err(Open::Incomplete);
        }
        pick(comp).as_deref().map_err(|open| *open)
    }

    /// Whether `cd`'s composition declines its property table.
    pub(crate) fn properties_open(&self, file: usize, cd: &ClassDecl) -> bool {
        self.composition_of(file, cd)
            .is_some_and(|c| self.table(&c, |c| &c.properties).is_err())
    }

    /// Whether a walk that reads `cd`'s own member lists must refuse `cd` for a member
    /// named `name`: its composition declines any table, or imports a method, a
    /// property or a constant of that name. The narrow obstacle for a consumer whose
    /// questions about the member (its root class, its declaring scope) the
    /// composition does not answer.
    pub(crate) fn trait_hides(&self, file: usize, cd: &ClassDecl, name: &str) -> bool {
        let Some(comp) = self.composition_of(file, cd) else { return false };
        let lname = name.to_ascii_lowercase();
        let tables: [(TablePick, &str); 3] =
            [(|c| &c.methods, lname.as_str()), (|c| &c.properties, name), (|c| &c.consts, name)];
        tables.iter().any(|(pick, key)| match self.table(&comp, *pick) {
            Err(_) => true,
            Ok(imported) => imported.binary_search_by(|i| (*i.name).cmp(key)).is_ok(),
        })
    }

    /// How a message names `method` of the trait `trait_fqn` (ADR-0105 §2.5): PHP names
    /// the importer (`C::f()`), so a trait one class imports, directly and alone, is
    /// named by that class; a trait several import, or none, keeps its own name, since
    /// the one walk of its body speaks for every importer.
    pub(crate) fn trait_method_display(&self, trait_fqn: &str, method: &str) -> String {
        let table = self.index.property_declarers(|| crate::site::declarers(self));
        let id = self.class_identity(trait_fqn);
        let importers: Vec<&String> = table
            .importers
            .get(&id)
            .map(|v| v.iter().collect())
            .unwrap_or_default();
        let owner = match importers.as_slice() {
            [only] if self.find_class(only).is_some_and(|(_, cd)| !cd.is_trait) => *only,
            _ => trait_fqn,
        };
        // The simple name, as a class method's message spells its class.
        let display = self.class_display_fqn(owner);
        let simple = display.rsplit('\\').next().unwrap_or(&display);
        format!("{simple}::{method}")
    }

    /// The trait declarations `cd`'s composition reaches, as `(file, decl)` pairs.
    pub(crate) fn reached_traits(&self, file: usize, cd: &ClassDecl) -> Vec<(usize, &'a ClassDecl)> {
        let Some(comp) = self.composition_of(file, cd) else { return Vec::new() };
        comp.reached
            .iter()
            .map(|&(f, i)| (f, &self.units[f].tree.classes()[i]))
            .collect()
    }

    fn imported_method(&self, holder: &'a ClassDecl, imp: &Imported) -> HeldMethod<'a> {
        let owner = &self.units[imp.member.file as usize].tree.classes()[imp.member.decl as usize];
        HeldMethod {
            decl: &owner.methods[imp.member.index as usize],
            owner,
            file: imp.member.file as usize,
            via: holder.used_traits.get(imp.via),
        }
    }

    /// The method `name` that `cd` (declared in `file`) holds: its own declaration
    /// (C1), else the one its traits give it (C2–C6). `Ok(None)` when it holds none,
    /// `Err` where the composition declines. Inherited methods are not read here.
    pub(crate) fn held_method(
        &self,
        file: usize,
        cd: &'a ClassDecl,
        name: &str,
    ) -> Result<Option<HeldMethod<'a>>, Open> {
        if let Some(m) = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(name)) {
            return Ok(Some(HeldMethod { decl: m, owner: cd, file, via: None }));
        }
        let Some(comp) = self.composition_of(file, cd) else { return Ok(None) };
        let imported = self.table(&comp, |c| &c.methods)?;
        let lname = name.to_ascii_lowercase();
        Ok(imported
            .binary_search_by(|i| (*i.name).cmp(lname.as_str()))
            .ok()
            .map(|at| self.imported_method(cd, &imported[at])))
    }

    /// Every method `cd` holds, its own in declaration order and then the imported
    /// ones by name; `Err` where the composition declines.
    pub(crate) fn held_methods(
        &self,
        file: usize,
        cd: &'a ClassDecl,
    ) -> Result<Vec<HeldMethod<'a>>, Open> {
        let mut out: Vec<HeldMethod<'a>> =
            cd.methods.iter().map(|m| HeldMethod { decl: m, owner: cd, file, via: None }).collect();
        if let Some(comp) = self.composition_of(file, cd) {
            let imported = self.table(&comp, |c| &c.methods)?;
            out.extend(imported.iter().map(|i| self.imported_method(cd, i)));
        }
        Ok(out)
    }

    /// The properties `cd` holds: its own and, after them, the imported ones (C11).
    pub(crate) fn held_properties(
        &self,
        file: usize,
        cd: &'a ClassDecl,
    ) -> Result<Vec<&'a PropertyDecl>, Open> {
        let mut out: Vec<&'a PropertyDecl> = cd.properties.iter().collect();
        if let Some(comp) = self.composition_of(file, cd) {
            let imported = self.table(&comp, |c| &c.properties)?;
            out.extend(imported.iter().map(|i| {
                let owner = &self.units[i.member.file as usize].tree.classes()[i.member.decl as usize];
                &owner.properties[i.member.index as usize]
            }));
        }
        Ok(out)
    }

    /// The property `name` that `cd` holds (C1, C11), case-sensitively as PHP reads it.
    pub(crate) fn held_property(
        &self,
        file: usize,
        cd: &'a ClassDecl,
        name: &str,
    ) -> Result<Option<&'a PropertyDecl>, Open> {
        if let Some(p) = cd.properties.iter().find(|p| p.name == name) {
            return Ok(Some(p));
        }
        Ok(self.held_properties(file, cd)?.into_iter().find(|p| p.name == name))
    }

    /// The declared visibility of the class constant `name` that `cd` holds (C1, C12).
    pub(crate) fn held_const(
        &self,
        file: usize,
        cd: &ClassDecl,
        name: &str,
    ) -> Result<Option<Visibility>, Open> {
        let read = |owner: &ClassDecl, at: usize| owner.const_visibility[at].1;
        if let Some(at) = cd.const_visibility.iter().position(|(n, _)| n == name) {
            return Ok(Some(read(cd, at)));
        }
        let Some(comp) = self.composition_of(file, cd) else { return Ok(None) };
        let imported = self.table(&comp, |c| &c.consts)?;
        Ok(imported.binary_search_by(|i| (*i.name).cmp(name)).ok().map(|at| {
            let i = &imported[at];
            let owner = &self.units[i.member.file as usize].tree.classes()[i.member.decl as usize];
            read(owner, i.member.index as usize)
        }))
    }
}
