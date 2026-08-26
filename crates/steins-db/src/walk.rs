//! The `.php` file walk — **the** one, used by the `steins` binary and by every
//! `xtask` harness that measures it (issue #524).
//!
//! Two rules, and the second is why this module exists at all.
//!
//! **A walk stays inside the tree it was pointed at.** A directory symlink is
//! followed only when its real target sits under one of the roots the caller
//! named, and every real directory is entered at most once. A link out of the
//! tree is not project code — following it analyzes files the user never asked
//! about, findings included — and a link back into the tree (`corpus/corpus ->
//! corpus`) is a re-entry that counts every file it reaches a second time. Both
//! are skipped, and both are *counted*: [`Sources::skipped_links`] is what
//! `doctor` reports, because a silently skipped path is how this class of bug
//! hides.
//!
//! A **file** symlink is followed. It is bounded (one file, no descent), it is
//! the plausible-intent case — a bootstrap or config file sourced from
//! elsewhere in a monorepo — and it is what naming that file on the command
//! line would do anyway. A file reachable under two spellings still collapses
//! to one entry: identity, not spelling, is what "one file" means (issue #179).
//!
//! Which spelling survives is decided, never accidental: entries are walked in
//! sorted order, and every real path in a directory is walked before any link
//! in it, so a file reachable both directly and through a link is reported by
//! its real path. Across the caller's roots the first root named still wins —
//! `steins check mirror src` reports `mirror/…`, as it has since #179.
//!
//! **One walk, one universe.** Before issue #524 the CLI and the perf harness
//! had separate collectors, which is how they came to disagree about what the
//! universe *was*: over the same 6,670-file tree the CLI reported 13,340 files
//! and `cargo xtask perf` reported 220,110, and every whole-corpus number taken
//! with the harness was inflated. An instrument that measures a different
//! universe than the product cannot inform the decisions it exists for, so both
//! come through here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Directory names no walk descends into. `.git` holds no analyzable source and
/// is large; pruning it by name (rather than by symlink rule) keeps the walk's
/// cost off the object store. Deliberately short: this list decides what is
/// *not analyzed*, and every entry on it is a silent omission.
const NEVER_DESCEND: &[&str] = &[".git"];

/// Why a symlink was not followed. Both variants are reportable posture, not
/// errors: the walk carries on either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSkip {
    /// The link's real target sits outside every root the caller named — code
    /// the run was not asked to analyze.
    Escapes,
    /// The link's real target is a directory this walk already entered — a
    /// re-entry that would count the same files twice.
    Revisits,
}

impl LinkSkip {
    /// The clause `doctor` prints after the count.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            LinkSkip::Escapes => "leaves the analyzed tree",
            LinkSkip::Revisits => "re-enters a directory already walked",
        }
    }
}

/// One directory symlink the walk refused, as spelled where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedLink {
    pub path: PathBuf,
    pub reason: LinkSkip,
}

/// What a walk found: the `.php` files, and the directory symlinks it refused.
#[derive(Debug, Default, Clone)]
pub struct Sources {
    /// The `.php` files, deduplicated by real identity and sorted.
    pub files: Vec<PathBuf>,
    /// The directory symlinks skipped, in encounter order.
    pub skipped_links: Vec<SkippedLink>,
}

impl Sources {
    /// `(escaping, re-entering)` counts — what `doctor` turns into a line.
    #[must_use]
    pub fn skip_counts(&self) -> (usize, usize) {
        let escapes =
            self.skipped_links.iter().filter(|s| s.reason == LinkSkip::Escapes).count();
        (escapes, self.skipped_links.len() - escapes)
    }
}

/// The `.php` files under `roots`, walked by the rules in the module docs.
///
/// Roots are honored as the caller spelled them, symlinks included: naming a
/// path *is* asking for it. The rules apply to what the descent discovers.
#[must_use]
pub fn php_files(roots: &[PathBuf]) -> Sources {
    Walk::new().run(roots)
}

/// A walk with caller-supplied pruning — the shape `fp-gate`'s local projects
/// need (`exclude` globs, a subtree-restricted walk whose boundary is still the
/// project root). Everything else takes [`php_files`].
#[derive(Default)]
pub struct Walk<'f> {
    /// Canonical boundary, when the caller's roots are not the boundary.
    confine: Option<Vec<PathBuf>>,
    prune_dir: Option<PathPredicate<'f>>,
    keep_file: Option<PathPredicate<'f>>,
}

/// A caller's yes/no about one path — [`Walk::pruning`], [`Walk::keeping`].
type PathPredicate<'f> = Box<dyn Fn(&Path) -> bool + 'f>;

impl<'f> Walk<'f> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Confine the walk to `root` rather than to the roots it is given. For a
    /// caller that walks *part* of a tree it owns (`fp-gate`'s per-project
    /// `paths`): a link from one listed subdirectory to another part of the
    /// same project stays inside the project, and only a link out of the
    /// project counts as leaving.
    #[must_use]
    pub fn confined_to(mut self, root: &Path) -> Self {
        self.confine.get_or_insert_with(Vec::new).push(root.to_path_buf());
        self
    }

    /// Skip a directory (and everything under it) when `f` says so. Called with
    /// the absolute-or-as-spelled path of the directory, before it is entered.
    #[must_use]
    pub fn pruning(mut self, f: impl Fn(&Path) -> bool + 'f) -> Self {
        self.prune_dir = Some(Box::new(f));
        self
    }

    /// Keep a `.php` file only when `f` says so.
    #[must_use]
    pub fn keeping(mut self, f: impl Fn(&Path) -> bool + 'f) -> Self {
        self.keep_file = Some(Box::new(f));
        self
    }

    /// Walk `roots` and return what was found.
    #[must_use]
    pub fn run(&self, roots: &[PathBuf]) -> Sources {
        let boundary: Vec<PathBuf> = self
            .confine
            .as_deref()
            .unwrap_or(roots)
            .iter()
            .filter_map(|r| r.canonicalize().ok())
            .collect();
        let mut state = State {
            boundary,
            visited: HashSet::new(),
            files: Vec::new(),
            skipped: Vec::new(),
        };
        for root in roots {
            // A root is walked as named — a symlinked root is what the caller
            // asked for — but it still enters through `enter_dir`, so two roots
            // naming one real directory are walked once, first spelling kept.
            if root.is_dir() {
                self.enter_dir(root, &mut state);
            } else {
                self.push_file(root, &mut state);
            }
        }
        Sources { files: dedup_by_identity(state.files), skipped_links: state.skipped }
    }

    /// Enter `dir` unless it is pruned by name/filter or already walked. The
    /// symlink verdict is the *caller's* (see [`Walk::admit_link`]) — by here
    /// the directory is one this walk is allowed to be in.
    fn enter_dir(&self, dir: &Path, state: &mut State) {
        if dir.file_name().is_some_and(|n| NEVER_DESCEND.iter().any(|s| n == *s)) {
            return;
        }
        if self.prune_dir.as_ref().is_some_and(|f| f(dir)) {
            return;
        }
        // Identity, not spelling: two roots that reach one real directory walk
        // it once. A directory that cannot be canonicalized (raced away,
        // unreadable parent) is walked uncached — `read_dir` then fails
        // harmlessly if it is really gone.
        if let Ok(canon) = dir.canonicalize()
            && !state.visited.insert(canon)
        {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        // Sorted, because `read_dir` order is the filesystem's and this walk
        // decides which *spelling* of a duplicated file survives — an answer
        // that must not depend on how a directory happens to be laid out.
        let mut listing: Vec<(PathBuf, std::fs::FileType)> = entries
            .flatten()
            // `file_type()` here does NOT follow the link — that is the whole
            // question — and on Unix it usually comes free with the dirent.
            .filter_map(|e| e.file_type().ok().map(|k| (e.path(), k)))
            .collect();
        listing.sort_by(|a, b| a.0.cmp(&b.0));

        // Real paths first, links after: a subtree reachable both ways is
        // walked — and so reported — under the spelling that is not a link.
        let mut links: Vec<PathBuf> = Vec::new();
        for (path, kind) in listing {
            if kind.is_symlink() {
                links.push(path);
            } else if kind.is_dir() {
                self.enter_dir(&path, state);
            } else {
                self.push_file(&path, state);
            }
        }
        for link in links {
            self.admit_link(&link, state);
        }
    }

    /// Decide a symlink. A link to a file is followed; a link to a directory is
    /// followed only if its real target is inside the boundary and has not been
    /// walked, and is recorded when it is not. A broken link names nothing and
    /// is neither followed nor reported.
    fn admit_link(&self, path: &Path, state: &mut State) {
        // `metadata` follows the link: what kind of thing is at the other end?
        let Ok(meta) = std::fs::metadata(path) else { return };
        if !meta.is_dir() {
            self.push_file(path, state);
            return;
        }
        let Ok(target) = path.canonicalize() else { return };
        if !state.boundary.iter().any(|root| target.starts_with(root)) {
            state.skipped.push(SkippedLink { path: path.to_path_buf(), reason: LinkSkip::Escapes });
            return;
        }
        if state.visited.contains(&target) {
            state
                .skipped
                .push(SkippedLink { path: path.to_path_buf(), reason: LinkSkip::Revisits });
            return;
        }
        // Inside the tree and not yet walked: walked through this spelling,
        // which is the one the user can see. `enter_dir` marks it visited, so
        // the real directory's own spelling is the one skipped if it comes later.
        self.enter_dir(path, state);
    }

    fn push_file(&self, path: &Path, state: &mut State) {
        if !path.extension().is_some_and(|e| e == "php") {
            return;
        }
        if self.keep_file.as_ref().is_some_and(|f| !f(path)) {
            return;
        }
        state.files.push(path.to_path_buf());
    }
}

struct State {
    /// Canonical roots the walk may not leave.
    boundary: Vec<PathBuf>,
    /// Canonical directories already entered.
    visited: HashSet<PathBuf>,
    files: Vec<PathBuf>,
    skipped: Vec<SkippedLink>,
}

/// Deduplicate by real identity, first spelling winning, then sort.
///
/// Issue #179: a file reachable two ways, ingested twice, double-declared its
/// classes and the absence family's existence guard (ADR-0049) read the
/// duplicated hierarchy as non-enumerable — findings vanished. Deduping by path
/// STRING does not catch it; the key is [`Path::canonicalize`], and a path that
/// cannot be canonicalized keys on itself so it is never silently dropped.
///
/// The walk's own rules make cross-spelling duplicates rare (a followed file
/// symlink, or overlapping root arguments), but "rare" is not "impossible" and
/// one `stat` per file is the wrong thing to economize on next to parsing it.
fn dedup_by_identity(files: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::with_capacity(files.len());
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let key = file.canonicalize().unwrap_or_else(|_| file.clone());
        if seen.insert(key) {
            out.push(file);
        }
    }
    // `read_dir` order is filesystem-dependent; the selection above is not.
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree with all three link shapes, plus the outside tree they can reach:
    ///
    /// ```text
    /// <dir>/outside/away.php
    /// <dir>/tree/src/a.php
    /// <dir>/tree/src/sub/b.php
    /// <dir>/tree/out    -> <dir>/outside        (directory, leaves the tree)
    /// <dir>/tree/self   -> <dir>/tree           (directory, re-enters)
    /// <dir>/tree/mirror -> <dir>/tree/src/sub   (directory, re-enters deeper)
    /// <dir>/tree/src/c.php -> <dir>/tree/src/a.php  (file, same tree)
    /// <dir>/tree/src/far.php -> <dir>/outside/away.php (file, outside)
    /// ```
    ///
    /// A human asked "how many files is that?" says three: `a.php`, `sub/b.php`
    /// and `far.php`. `c.php` is `a.php` under another name.
    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("steins-walk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        std::fs::create_dir_all(dir.join("tree/src/sub")).unwrap();
        std::fs::write(dir.join("outside/away.php"), "<?php\n").unwrap();
        std::fs::write(dir.join("tree/src/a.php"), "<?php\n").unwrap();
        std::fs::write(dir.join("tree/src/sub/b.php"), "<?php\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(dir.join("outside"), dir.join("tree/out")).unwrap();
            symlink(dir.join("tree"), dir.join("tree/self")).unwrap();
            symlink(dir.join("tree/src/sub"), dir.join("tree/mirror")).unwrap();
            symlink(dir.join("tree/src/a.php"), dir.join("tree/src/c.php")).unwrap();
            symlink(dir.join("outside/away.php"), dir.join("tree/src/far.php")).unwrap();
        }
        dir
    }

    #[cfg(unix)]
    #[test]
    fn the_walk_counts_what_a_human_would_count() {
        let dir = fixture("human");
        let found = php_files(&[dir.join("tree")]);

        let names: Vec<String> = found
            .files
            .iter()
            .map(|f| f.strip_prefix(dir.join("tree")).unwrap().display().to_string())
            .collect();
        assert_eq!(names, vec!["src/a.php", "src/far.php", "src/sub/b.php"], "got {names:?}");

        // Two directory links re-enter (`self`, `mirror`), one leaves (`out`).
        assert_eq!(found.skip_counts(), (1, 2), "skipped: {:?}", found.skipped_links);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The link out of the tree is not analyzed *and* not silent — the count is
    /// what `doctor` reports.
    #[cfg(unix)]
    #[test]
    fn an_escaping_link_is_skipped_and_named() {
        let dir = fixture("escape");
        let found = php_files(&[dir.join("tree")]);
        let escaped: Vec<&SkippedLink> =
            found.skipped_links.iter().filter(|s| s.reason == LinkSkip::Escapes).collect();
        assert_eq!(escaped.len(), 1);
        assert_eq!(escaped[0].path, dir.join("tree/out"));
        assert!(
            !found.files.iter().any(|f| f.starts_with(dir.join("tree/out"))),
            "nothing under the escaping link is analyzed, got {:?}",
            found.files
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Naming the outside tree makes it inside: the boundary is the roots the
    /// caller named, not a rule about where files may live. The universe is
    /// still three real files — `away.php` is reachable three ways now.
    #[cfg(unix)]
    #[test]
    fn naming_the_far_tree_admits_the_link_to_it() {
        let dir = fixture("named");
        let found = php_files(&[dir.join("tree"), dir.join("outside")]);
        let mut real: Vec<PathBuf> =
            found.files.iter().map(|f| f.canonicalize().unwrap()).collect();
        real.sort();
        assert_eq!(
            real,
            vec![
                dir.join("outside/away.php").canonicalize().unwrap(),
                dir.join("tree/src/a.php").canonicalize().unwrap(),
                dir.join("tree/src/sub/b.php").canonicalize().unwrap(),
            ],
            "got {:?}",
            found.files
        );
        // `tree/out` is inside the named roots now, so it is not an escape.
        assert_eq!(found.skip_counts().0, 0, "skipped: {:?}", found.skipped_links);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A root that IS a symlink is walked: naming a path is asking for it.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_is_walked() {
        let dir = fixture("root");
        let found = php_files(&[dir.join("tree/mirror")]);
        assert_eq!(found.files, vec![dir.join("tree/mirror/b.php")], "got {:?}", found.files);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `corpus/corpus -> corpus`, the issue's own shape: the count is the tree's,
    /// not twice the tree's, and the walk terminates.
    #[cfg(unix)]
    #[test]
    fn a_self_link_does_not_double_the_tree() {
        let dir = std::env::temp_dir().join(format!("steins-walk-self-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("corpus/pkg")).unwrap();
        std::fs::write(dir.join("corpus/pkg/a.php"), "<?php\n").unwrap();
        std::fs::write(dir.join("corpus/pkg/b.php"), "<?php\n").unwrap();
        std::os::unix::fs::symlink(dir.join("corpus"), dir.join("corpus/corpus")).unwrap();

        let found = php_files(&[dir.join("corpus")]);
        assert_eq!(found.files.len(), 2, "got {:?}", found.files);
        assert_eq!(found.skip_counts(), (0, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two roots reaching one real tree: the files come back once, spelled as
    /// the first root reached them (issue #179's argument-order rule).
    #[cfg(unix)]
    #[test]
    fn overlapping_roots_keep_the_first_spelling() {
        let dir = fixture("overlap");
        let mirror_first = php_files(&[dir.join("tree/mirror"), dir.join("tree/src/sub")]);
        assert_eq!(mirror_first.files, vec![dir.join("tree/mirror/b.php")]);
        let sub_first = php_files(&[dir.join("tree/src/sub"), dir.join("tree/mirror")]);
        assert_eq!(sub_first.files, vec![dir.join("tree/src/sub/b.php")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path that names nothing is not walked and not fatal — callers
    /// diagnose a missing argument themselves (ADR-0050 §7 amendment).
    #[test]
    fn a_missing_root_yields_nothing() {
        let found = php_files(&[PathBuf::from("/steins-walk-does-not-exist")]);
        assert!(found.files.is_empty() && found.skipped_links.is_empty());
    }

    /// A file named directly is taken even though no walk reaches it.
    #[test]
    fn a_file_root_is_taken_as_itself() {
        let dir = std::env::temp_dir().join(format!("steins-walk-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.php"), "<?php\n").unwrap();
        std::fs::write(dir.join("b.txt"), "x\n").unwrap();
        let found = php_files(&[dir.join("a.php"), dir.join("b.txt")]);
        assert_eq!(found.files, vec![dir.join("a.php")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The caller's own pruning composes with the symlink rules.
    #[test]
    fn pruning_and_keeping_filters_apply() {
        let dir = std::env::temp_dir().join(format!("steins-walk-filter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/skipme")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("src/a.php"), "<?php\n").unwrap();
        std::fs::write(dir.join("src/no.php"), "<?php\n").unwrap();
        std::fs::write(dir.join("src/skipme/c.php"), "<?php\n").unwrap();
        std::fs::write(dir.join(".git/hook.php"), "<?php\n").unwrap();

        let found = Walk::new()
            .pruning(|d: &Path| d.file_name().is_some_and(|n| n == "skipme"))
            .keeping(|f: &Path| !f.file_name().is_some_and(|n| n == "no.php"))
            .run(std::slice::from_ref(&dir));
        assert_eq!(found.files, vec![dir.join("src/a.php")], "got {:?}", found.files);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `dedup_by_identity` never drops a path it cannot canonicalize.
    #[test]
    fn dedup_keeps_uncanonicalizable_paths() {
        let a = PathBuf::from("/steins-walk-unit-does-not-exist-a.php");
        let b = PathBuf::from("/steins-walk-unit-does-not-exist-b.php");
        let out = dedup_by_identity(vec![a.clone(), b.clone(), a.clone()]);
        assert_eq!(out, vec![a, b]);
    }
}
