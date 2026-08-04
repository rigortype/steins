//! The project layout: which trees are ours and which are somebody else's
//! (ADR-0015).
//!
//! Vendor classification decides whether a finding is reported at all, whether a
//! declaration is a transform candidate, and whether an `eval` gets the vendor
//! presumption (ADR-0046 §2). Getting it wrong silently reattributes findings
//! between "ours" and "theirs", so the answer must come from what the project
//! *declares*, not from a directory-name guess.
//!
//! The guess is [`fallback_is_vendor`]: a path is vendor when any component is
//! literally `vendor`. It is right for the common Composer install and wrong in
//! both directions elsewhere — a tree vendoring into `3rdparty/` is disowned by
//! nobody, and a first-party `src/vendor/` is disowned by us.
//!
//! # Layering
//!
//! [`ProjectLayout`] is pure: no IO, no syscalls, no ambient state. Everything it
//! compares against is captured at construction, including the working directory,
//! so a replay with the same inputs gives the same answer (ADR-0048's canonical
//! entry state). [`crate::composer::discover`] is the boundary that reads the
//! filesystem; it runs once per run, before any salsa input is set.
//!
//! # The rule
//!
//! Each `composer.json` found under the analyzed paths declares a *governing
//! root*: its own directory, its vendor directory (`config.vendor-dir`, default
//! `vendor`), and its first-party roots (the `autoload` / `autoload-dev` PSR-4,
//! PSR-0 and classmap directories). A path is governed by the **nearest** such
//! root above it. Then, longest component-prefix wins:
//!
//! - a declared vendor root beats a first-party root → vendor;
//! - a first-party root that is at least as specific → not vendor (this is what
//!   stops a first-party `src/vendor/` from being disowned);
//! - neither matches → [`fallback_is_vendor`].
//!
//! The fallback survives as the floor deliberately. A monorepo carries vendor
//! trees under subprojects whose manifests are not checked in, and a rule that
//! *only* honoured declarations would hand every one of them back to the project
//! as first-party code.

use std::path::{Component, Path, PathBuf};

/// Where a resolved [`PhpTarget`] came from, in precedence order (issue #28):
/// `config.platform.php` is Composer's own "resolve as if on this PHP" pin and
/// beats the `require.php` constraint when both are present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhpTargetSource {
    /// `config.platform.php` — a concrete version Composer resolves against.
    Platform,
    /// `require.php` — the declared support constraint; the floor is the target.
    Require,
}

impl PhpTargetSource {
    /// The manifest spelling, for `doctor`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            PhpTargetSource::Platform => "config.platform.php",
            PhpTargetSource::Require => "require.php",
        }
    }
}

/// The **target PHP version range** a project declares (issue #28), in
/// `(major, minor)` space — the version the analysis is *about*, as distinct
/// from the version the sidecar happens to run.
///
/// A ceiling of `Some((8, u16::MAX))` spells "any minor of major 8" (what
/// `^8.1` means); `None` spells an open upper bound (`>=8.1`). Patch levels are
/// deliberately dropped: every version-sensitive decision Steins makes keys on
/// the minor (ADR-0049 A12, ADR-0052 A11, ADR-0056 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhpTarget {
    /// The lowest `(major, minor)` the project declares support for.
    pub floor: (u16, u16),
    /// The highest declared `(major, minor)`, inclusive; `None` when open.
    pub ceiling: Option<(u16, u16)>,
    /// Which manifest field produced this target.
    pub source: PhpTargetSource,
    /// The constraint text as written, for `doctor`.
    pub raw: String,
}

impl PhpTarget {
    /// Whether `minor` lies inside the declared range.
    #[must_use]
    pub fn contains(&self, minor: (u16, u16)) -> bool {
        self.floor <= minor && self.ceiling.is_none_or(|c| minor <= c)
    }

    /// Whether the range is exactly the single minor `m`.
    #[must_use]
    pub fn is_exactly(&self, m: (u16, u16)) -> bool {
        self.floor == m && self.ceiling == Some(m)
    }

    /// Whether the range spans versions on both sides of `boundary` — i.e. some
    /// declared minor is below it and some declared (or open-bound) minor is at
    /// or above it. This is what generalizes ADR-0049 A12's per-literal unknown
    /// leg to a range: a rule keyed on `boundary` has no single answer for a
    /// straddling target, so a boundary-sensitive question must decline.
    #[must_use]
    pub fn straddles(&self, boundary: (u16, u16)) -> bool {
        self.floor < boundary && self.ceiling.is_none_or(|c| c >= boundary)
    }

    /// Render the resolved range for `doctor`: `8.1+`, `8.1–8.3`, `8.1 (8.x)`,
    /// `8.1 (exact)`.
    #[must_use]
    pub fn render(&self) -> String {
        let f = format!("{}.{}", self.floor.0, self.floor.1);
        match self.ceiling {
            None => format!("{f}+"),
            Some(c) if c == self.floor => format!("{f} (exact)"),
            Some((maj, m)) if m == u16::MAX => format!("{f} ({maj}.x)"),
            Some((maj, m)) => format!("{f}\u{2013}{maj}.{m}"),
        }
    }
}

/// One `composer.json` and the roots it declares. Paths are absolute and
/// lexically normalized (see [`normalize`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoverningRoot {
    /// The manifest's own directory — what this root governs.
    dir: PathBuf,
    /// The manifest that declared it, kept for `doctor`'s benefit.
    manifest: PathBuf,
    /// Vendor roots: `config.vendor-dir` resolved against `dir`.
    vendor: Vec<PathBuf>,
    /// First-party roots: the autoload PSR-4 / PSR-0 / classmap directories.
    first_party: Vec<PathBuf>,
    /// The target PHP range this manifest declares (issue #28), when it does.
    php_target: Option<PhpTarget>,
}

impl GoverningRoot {
    /// Build a root from an already-parsed manifest. `dir` is the manifest's
    /// directory; both root lists are resolved against it.
    #[must_use]
    pub fn new(manifest: PathBuf, dir: PathBuf, vendor: Vec<PathBuf>, first_party: Vec<PathBuf>) -> Self {
        Self { dir, manifest, vendor, first_party, php_target: None }
    }

    /// Attach the manifest's declared PHP target (issue #28).
    #[must_use]
    pub fn with_php_target(mut self, target: Option<PhpTarget>) -> Self {
        self.php_target = target;
        self
    }

    /// The target PHP range this manifest declares, when it does.
    #[must_use]
    pub fn php_target(&self) -> Option<&PhpTarget> {
        self.php_target.as_ref()
    }

    /// The directory this root governs.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The `composer.json` that declared it.
    #[must_use]
    pub fn manifest(&self) -> &Path {
        &self.manifest
    }

    /// The declared vendor roots.
    #[must_use]
    pub fn vendor_roots(&self) -> &[PathBuf] {
        &self.vendor
    }

    /// The declared first-party roots.
    #[must_use]
    pub fn first_party_roots(&self) -> &[PathBuf] {
        &self.first_party
    }

    /// The component depth of the governed directory — the specificity a declared
    /// root must exceed to say anything narrower than "the whole project".
    #[must_use]
    fn depth(&self) -> usize {
        self.dir.components().count()
    }
}

/// The resolved layout of one run: the working directory every relative path is
/// read against, and the governing roots discovered under the analyzed paths.
///
/// [`ProjectLayout::fallback`] is the no-manifest layout — every question falls
/// through to [`fallback_is_vendor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLayout {
    /// The directory a relative analyzed path is resolved against. Captured, not
    /// read from the environment on demand: the answer must not depend on when
    /// it is asked.
    cwd: PathBuf,
    /// Governing roots, ordered deepest-first so the nearest ancestor is the
    /// first match.
    roots: Vec<GoverningRoot>,
}

impl Default for ProjectLayout {
    fn default() -> Self {
        Self::fallback()
    }
}

impl ProjectLayout {
    /// The layout with no declarations: every question answered by
    /// [`fallback_is_vendor`]. The working directory is irrelevant here (nothing
    /// is compared against a root), so it is empty.
    #[must_use]
    pub fn fallback() -> Self {
        Self { cwd: PathBuf::new(), roots: Vec::new() }
    }

    /// Build a layout from already-discovered roots. Orders them deepest-first so
    /// the private `governing_root` lookup can take the first match.
    #[must_use]
    pub fn new(cwd: PathBuf, mut roots: Vec<GoverningRoot>) -> Self {
        roots.sort_by(|a, b| {
            let depth = |r: &GoverningRoot| r.dir.components().count();
            depth(b).cmp(&depth(a)).then_with(|| b.dir.cmp(&a.dir))
        });
        Self { cwd, roots }
    }

    /// The discovered governing roots, deepest-first.
    #[must_use]
    pub fn roots(&self) -> &[GoverningRoot] {
        &self.roots
    }

    /// Whether this layout carries no declarations, so every answer is the
    /// fallback guess. `doctor` says so out loud: a run that guessed and a run
    /// that read the project's own manifest are different claims.
    #[must_use]
    pub fn is_fallback(&self) -> bool {
        self.roots.is_empty()
    }

    /// The analysis's **target PHP range** (issue #28): the declaration of the
    /// *outermost* governing root — the top-level project, whose
    /// `config.platform.php` / `require.php` describes what the whole tree
    /// deploys on. Nested manifests (monorepo subprojects, vendored packages)
    /// deliberately do not override it: a library's `^7.4` support claim does
    /// not change what the application ships on. Roots are kept deepest-first,
    /// so the outermost is the last; ties (sibling projects analyzed together)
    /// resolve to the sort order's last, and `doctor` names the manifest.
    #[must_use]
    pub fn php_target(&self) -> Option<&PhpTarget> {
        self.roots.last().and_then(GoverningRoot::php_target)
    }

    /// Whether `path` — an analyzed file's path, absolute or relative to the
    /// captured working directory — belongs to a vendor tree.
    ///
    /// See the module docs for the rule. A path governed by no root falls through
    /// to [`fallback_is_vendor`], and so does every path when the layout is the
    /// fallback one.
    #[must_use]
    pub fn is_vendor(&self, path: &str) -> bool {
        if self.roots.is_empty() {
            return fallback_is_vendor(path);
        }
        let abs = self.absolutize(path);
        let Some(root) = self.governing_root(&abs) else {
            return fallback_is_vendor(path);
        };
        let declared = longest_prefix(&root.vendor, &abs);
        let first = longest_prefix(&root.first_party, &abs);
        // `Option`'s own ordering does the work: `None` is less than every
        // `Some`, so an unmatched side always loses to a matched one.
        if declared.is_some() && declared > first {
            return true;
        }
        // A first-party root defends a path from the floor only when it names
        // something *narrower than the project itself*. `autoload: {"": "./"}`
        // claims the whole tree, and honouring that claim would hand every
        // undeclared nested vendor tree back to the project as its own code.
        if first.is_some_and(|n| n > root.depth()) && first >= declared {
            return false;
        }
        fallback_is_vendor(path)
    }

    /// The nearest governing root above `abs`, or `None` when no manifest governs
    /// it. Roots are deepest-first, so the first component-prefix match is it.
    fn governing_root(&self, abs: &Path) -> Option<&GoverningRoot> {
        self.roots.iter().find(|r| prefix_len(&r.dir, abs).is_some())
    }

    /// Resolve `path` against the captured working directory and normalize it
    /// lexically. No syscalls: a symlink is not followed, and a path that does
    /// not exist resolves the same as one that does.
    fn absolutize(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() { normalize(p) } else { normalize(&self.cwd.join(p)) }
    }
}

/// The directory-name predicate: a path is vendor when any component is literally
/// `vendor` (ADR-0015). Serves as the documented floor under [`ProjectLayout`], and
/// as the whole answer when no `composer.json` governs a path.
#[must_use]
pub fn fallback_is_vendor(path: &str) -> bool {
    path.split(['/', '\\']).any(|component| component == "vendor")
}

/// The number of components of `base` when `base` is a component-prefix of
/// `path`, else `None`. Component-wise, so `/a/bc` is not a prefix of `/a/bcd`.
fn prefix_len(base: &Path, path: &Path) -> Option<usize> {
    let mut n = 0;
    let mut p = path.components();
    for b in base.components() {
        if p.next()? != b {
            return None;
        }
        n += 1;
    }
    Some(n)
}

/// The longest component-prefix among `bases` that covers `path`.
fn longest_prefix(bases: &[PathBuf], path: &Path) -> Option<usize> {
    bases.iter().filter_map(|b| prefix_len(b, path)).max()
}

/// Lexically normalize a path: drop `.`, pop a component for each `..` that has
/// one to pop, and keep everything else. Purely textual — no filesystem access,
/// so it never follows a symlink and never fails.
#[must_use]
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(dir: &str, vendor: &[&str], first: &[&str]) -> GoverningRoot {
        let dir = PathBuf::from(dir);
        GoverningRoot::new(
            dir.join("composer.json"),
            dir.clone(),
            vendor.iter().map(|v| dir.join(v)).collect(),
            first.iter().map(|f| dir.join(f)).collect(),
        )
    }

    fn layout(roots: Vec<GoverningRoot>) -> ProjectLayout {
        ProjectLayout::new(PathBuf::from("/proj"), roots)
    }

    #[test]
    fn fallback_matches_the_historical_predicate() {
        assert!(fallback_is_vendor("vendor/foo/Bar.php"));
        assert!(fallback_is_vendor("src/vendor/foo/Bar.php"));
        assert!(fallback_is_vendor("/abs/mono/vendor/pkg/lib.php"));
        assert!(!fallback_is_vendor("src/App/Bar.php"));
        // A layout with no roots answers every question with it.
        let l = ProjectLayout::fallback();
        assert!(l.is_vendor("vendor/foo/Bar.php"));
        assert!(!l.is_vendor("src/App/Bar.php"));
        assert!(l.is_fallback());
    }

    #[test]
    fn a_declared_vendor_dir_that_is_not_named_vendor_is_vendor() {
        // The nextcloud shape: `config.vendor-dir: 3rdparty`.
        let l = layout(vec![root("/proj", &["3rdparty"], &["lib"])]);
        assert!(l.is_vendor("/proj/3rdparty/pkg/Lib.php"));
        assert!(!l.is_vendor("/proj/lib/App.php"));
    }

    #[test]
    fn a_first_party_root_is_not_disowned_by_the_fallback() {
        // `src/vendor/` is ours: the autoload root covers it and no declared
        // vendor root is more specific.
        let l = layout(vec![root("/proj", &["vendor"], &["src"])]);
        assert!(!l.is_vendor("/proj/src/vendor/Money.php"));
        assert!(l.is_vendor("/proj/vendor/pkg/Lib.php"));
    }

    #[test]
    fn a_vendor_root_beats_a_less_specific_first_party_root() {
        // `autoload: {"": "./"}` maps the whole project; the vendor dir is still
        // vendor because it is the longer prefix.
        let l = layout(vec![root("/proj", &["vendor"], &[""])]);
        assert!(l.is_vendor("/proj/vendor/pkg/Lib.php"));
        assert!(!l.is_vendor("/proj/src/App.php"));
    }

    #[test]
    fn a_whole_tree_autoload_root_does_not_defend_an_undeclared_vendor_tree() {
        // `autoload: {"psr-4": {"": "./"}}` claims everything. It must not turn an
        // undeclared nested vendor tree into first-party code.
        let l = layout(vec![root("/proj", &["vendor"], &[""])]);
        assert!(l.is_vendor("/proj/other/vendor/pkg/Lib.php"));
        assert!(l.is_vendor("/proj/vendor/pkg/Lib.php"));
        assert!(!l.is_vendor("/proj/src/App.php"));
    }

    #[test]
    fn an_undeclared_vendor_tree_still_falls_back() {
        // The monorepo shape: a subproject whose manifest is not checked in. The
        // governing root declares nothing about it, so the floor decides — which
        // is why the floor is kept.
        let l = layout(vec![root("/proj", &["vendor"], &["src"])]);
        assert!(l.is_vendor("/proj/other/vendor/pkg/Lib.php"));
    }

    #[test]
    fn the_nearest_manifest_governs() {
        let l = layout(vec![
            root("/proj", &["vendor"], &["src"]),
            root("/proj/sub", &["deps"], &["lib"]),
        ]);
        assert!(l.is_vendor("/proj/sub/deps/pkg/Lib.php"));
        assert!(!l.is_vendor("/proj/sub/lib/App.php"));
        // The outer root's vendor dir does not reach into the inner project.
        assert!(!l.is_vendor("/proj/sub/lib/vendor_helpers/Money.php"));
    }

    #[test]
    fn a_relative_path_resolves_against_the_captured_cwd() {
        let l = layout(vec![root("/proj", &["3rdparty"], &["lib"])]);
        assert!(l.is_vendor("3rdparty/pkg/Lib.php"));
        assert!(l.is_vendor("./3rdparty/pkg/Lib.php"));
        assert!(!l.is_vendor("lib/App.php"));
    }

    #[test]
    fn normalization_is_lexical_and_total() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("a/b/../../../c")), PathBuf::from("../c"));
    }

    #[test]
    fn a_path_outside_every_root_falls_back() {
        let l = layout(vec![root("/proj", &["vendor"], &["src"])]);
        assert!(l.is_vendor("/elsewhere/vendor/pkg/Lib.php"));
        assert!(!l.is_vendor("/elsewhere/src/App.php"));
    }
}
