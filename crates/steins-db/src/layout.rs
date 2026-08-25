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
//! literally `vendor`. Right for the common Composer install, wrong in both
//! directions elsewhere — a `3rdparty/` tree is disowned by nobody, a
//! first-party `src/vendor/` is disowned by us.
//!
//! **Layering:** [`ProjectLayout`] is pure — no IO, no syscalls, no ambient
//! state. Everything it compares against, including the working directory, is
//! captured at construction, so a replay with the same inputs gives the same
//! answer (ADR-0048). [`crate::composer::discover`] is the boundary that reads
//! the filesystem, once per run, before any salsa input is set.
//!
//! **The rule:** each `composer.json` under the analyzed paths declares a
//! *governing root* — its directory, vendor directory (`config.vendor-dir`,
//! default `vendor`), and first-party roots (`autoload`/`autoload-dev` PSR-4,
//! PSR-0, classmap directories). A path is governed by the **nearest** such
//! root above it, then longest component-prefix wins: a declared vendor root
//! beats a first-party root → vendor; a first-party root at least as specific →
//! not vendor (stops `src/vendor/` from being disowned); neither →
//! [`fallback_is_vendor`]. The fallback survives as the floor deliberately: a
//! monorepo carries vendor trees under subprojects whose manifests aren't
//! checked in, and honoring only declarations would hand those back as
//! first-party code.
//!
//! **The no-manifest config channel (issue #181):** a project with no
//! `composer.json` has no `vendor-dir` to read, so its own third-party tree
//! (`3rdparty/`, `lib/vendor/`, …) never gets to be anything but first-party.
//! `steins.toml`'s `[paths] vendor-dirs` names extra whole-path-component
//! sequences to treat as the floor alongside `vendor`
//! ([`ProjectLayout::with_extra_vendor_dirs`]) — consulted only where
//! [`fallback_is_vendor`] already was, never where a declared vendor dir
//! answered, so a project with a `composer.json` needs it not at all.

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
/// `(major, minor)` space — the version the analysis is *about*, distinct from
/// the version the sidecar happens to run.
///
/// A ceiling of `Some((8, u16::MAX))` spells "any minor of major 8" (`^8.1`);
/// `None` spells an open upper bound (`>=8.1`). Patch levels are dropped: every
/// version-sensitive decision keys on the minor (ADR-0049 A12, ADR-0052 A11,
/// ADR-0056 §2).
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

    /// Whether the range spans versions on both sides of `boundary`. Generalizes
    /// ADR-0049 A12's per-literal unknown leg to a range: a boundary-sensitive
    /// question has no single answer for a straddling target and must decline.
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
    /// `steins.toml [paths] vendor-dirs` (issue #181), pre-split into component
    /// sequences at construction — see [`ProjectLayout::with_extra_vendor_dirs`].
    /// Empty unless the caller set it, so a project with no such config answers
    /// exactly as before.
    extra_vendor_dirs: Vec<Vec<String>>,
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
        Self { cwd: PathBuf::new(), roots: Vec::new(), extra_vendor_dirs: Vec::new() }
    }

    /// Build a layout from already-discovered roots. Orders them deepest-first so
    /// the private `governing_root` lookup can take the first match.
    #[must_use]
    pub fn new(cwd: PathBuf, mut roots: Vec<GoverningRoot>) -> Self {
        roots.sort_by(|a, b| {
            let depth = |r: &GoverningRoot| r.dir.components().count();
            depth(b).cmp(&depth(a)).then_with(|| b.dir.cmp(&a.dir))
        });
        Self { cwd, roots, extra_vendor_dirs: Vec::new() }
    }

    /// Attach `steins.toml [paths] vendor-dirs` (issue #181): extra directory-name
    /// sequences to treat as vendor at the floor, alongside the `vendor` literal.
    /// Each entry is a `/`-separated component sequence (`"3rdparty"`,
    /// `"lib/vendor"`) matched as a contiguous whole-component run, the same
    /// discipline [`fallback_is_vendor`] uses (so `vendor_proj/` never matches
    /// and an empty entry matches nothing). Unset, a Composer project stays
    /// zero-config.
    #[must_use]
    pub fn with_extra_vendor_dirs(mut self, dirs: Vec<String>) -> Self {
        self.extra_vendor_dirs = dirs
            .into_iter()
            .map(|d| d.split(['/', '\\']).filter(|c| !c.is_empty()).map(str::to_owned).collect::<Vec<_>>())
            .filter(|components| !components.is_empty())
            .collect();
        self
    }

    /// The discovered governing roots, deepest-first.
    #[must_use]
    pub fn roots(&self) -> &[GoverningRoot] {
        &self.roots
    }

    /// The captured working directory — what the partition classifier
    /// (`crate::partition`) resolves relative paths against, so its answers
    /// and [`ProjectLayout::is_vendor`]'s agree on the same spelling.
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Whether this layout carries no declarations, so every answer is the
    /// fallback guess. `doctor` says so out loud: a run that guessed and a run
    /// that read the project's own manifest are different claims.
    #[must_use]
    pub fn is_fallback(&self) -> bool {
        self.roots.is_empty()
    }

    /// The analysis's **target PHP range** (issue #28): the *outermost*
    /// governing root's declaration — the top-level project, whose
    /// `config.platform.php`/`require.php` describes what the whole tree
    /// deploys on. Nested manifests (monorepo subprojects, vendored packages)
    /// deliberately do not override it. Roots are kept deepest-first, so the
    /// outermost is last; ties (sibling projects) resolve to the sort order's
    /// last, and `doctor` names the manifest.
    #[must_use]
    pub fn php_target(&self) -> Option<&PhpTarget> {
        self.roots.last().and_then(GoverningRoot::php_target)
    }

    /// Whether `path` — an analyzed file's path, absolute or relative to the
    /// captured working directory — belongs to a vendor tree. See the module
    /// docs for the rule.
    #[must_use]
    pub fn is_vendor(&self, path: &str) -> bool {
        if self.roots.is_empty() {
            return self.floor(path);
        }
        let abs = self.absolutize(path);
        let Some(root) = self.governing_root(&abs) else {
            return self.floor(path);
        };
        let declared = longest_prefix(&root.vendor, &abs);
        let first = longest_prefix(&root.first_party, &abs);
        // `Option`'s ordering: `None` < every `Some`, so an unmatched side
        // always loses to a matched one.
        if declared.is_some() && declared > first {
            return true;
        }
        // A first-party root defends a path only when narrower than the project
        // itself — `autoload: {"": "./"}` must not hand an undeclared nested
        // vendor tree back as first-party code.
        if first.is_some_and(|n| n > root.depth()) && first >= declared {
            return false;
        }
        self.floor(path)
    }

    /// The floor every unanswered question falls to: the `vendor` literal
    /// ([`fallback_is_vendor`]) or a whole-component match against a declared
    /// `steins.toml [paths] vendor-dirs` sequence (issue #181).
    fn floor(&self, path: &str) -> bool {
        fallback_is_vendor(path) || self.matches_extra_vendor_dir(path)
    }

    /// Whether `path` contains, as a contiguous run, every component of any
    /// declared `[paths] vendor-dirs` entry (component-wise, like
    /// [`fallback_is_vendor`]).
    fn matches_extra_vendor_dir(&self, path: &str) -> bool {
        if self.extra_vendor_dirs.is_empty() {
            return false;
        }
        let components: Vec<&str> = path.split(['/', '\\']).collect();
        self.extra_vendor_dirs.iter().any(|entry| {
            components.windows(entry.len()).any(|w| w.iter().zip(entry).all(|(c, e)| *c == e))
        })
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
        // Whole-component matching, not substring (issue #181).
        assert!(!fallback_is_vendor("vendor_proj/Bar.php"));
        assert!(!fallback_is_vendor("src/vendor.php"));
        let l = ProjectLayout::fallback();
        assert!(l.is_vendor("vendor/foo/Bar.php"));
        assert!(!l.is_vendor("src/App/Bar.php"));
        assert!(!l.is_vendor("vendor_proj/Bar.php"));
        assert!(!l.is_vendor("src/vendor.php"));
        assert!(l.is_fallback());
    }

    #[test]
    fn a_declared_vendor_dir_that_is_not_named_vendor_is_vendor() {
        let l = layout(vec![root("/proj", &["3rdparty"], &["lib"])]);
        assert!(l.is_vendor("/proj/3rdparty/pkg/Lib.php"));
        assert!(!l.is_vendor("/proj/lib/App.php"));
    }

    #[test]
    fn a_first_party_root_is_not_disowned_by_the_fallback() {
        let l = layout(vec![root("/proj", &["vendor"], &["src"])]);
        assert!(!l.is_vendor("/proj/src/vendor/Money.php"));
        assert!(l.is_vendor("/proj/vendor/pkg/Lib.php"));
    }

    #[test]
    fn a_vendor_root_beats_a_less_specific_first_party_root() {
        let l = layout(vec![root("/proj", &["vendor"], &[""])]);
        assert!(l.is_vendor("/proj/vendor/pkg/Lib.php"));
        assert!(!l.is_vendor("/proj/src/App.php"));
    }

    #[test]
    fn a_whole_tree_autoload_root_does_not_defend_an_undeclared_vendor_tree() {
        let l = layout(vec![root("/proj", &["vendor"], &[""])]);
        assert!(l.is_vendor("/proj/other/vendor/pkg/Lib.php"));
        assert!(l.is_vendor("/proj/vendor/pkg/Lib.php"));
        assert!(!l.is_vendor("/proj/src/App.php"));
    }

    #[test]
    fn an_undeclared_vendor_tree_still_falls_back() {
        // Monorepo shape: subproject manifest not checked in, so the floor decides.
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

    #[test]
    fn extra_vendor_dirs_extend_the_no_manifest_floor() {
        let l = ProjectLayout::fallback().with_extra_vendor_dirs(vec!["3rdparty".to_owned()]);
        assert!(l.is_vendor("3rdparty/pkg/Lib.php"));
        assert!(l.is_vendor("app/3rdparty/pkg/Lib.php"));
        assert!(l.is_vendor("vendor/pkg/Lib.php"));
        assert!(!l.is_vendor("src/App.php"));
    }

    #[test]
    fn extra_vendor_dirs_are_whole_component_sequences() {
        let l = ProjectLayout::fallback().with_extra_vendor_dirs(vec!["lib/deps".to_owned()]);
        assert!(l.is_vendor("lib/deps/pkg/Lib.php"));
        assert!(!l.is_vendor("lib/other/deps/pkg/Lib.php"));
        assert!(!l.is_vendor("other/lib/App.php"));
    }

    #[test]
    fn extra_vendor_dirs_never_match_a_component_prefix_or_suffix() {
        let l = ProjectLayout::fallback().with_extra_vendor_dirs(vec!["3rdparty".to_owned()]);
        assert!(!l.is_vendor("3rdparty_extra/Lib.php"));
        assert!(!l.is_vendor("my3rdparty/Lib.php"));
        assert!(!l.is_vendor("3rdparty.php"));
    }

    #[test]
    fn extra_vendor_dirs_only_reach_paths_no_declared_root_answers() {
        let l =
            layout(vec![root("/proj", &["vendor"], &["src"])]).with_extra_vendor_dirs(vec!["src".to_owned()]);
        assert!(!l.is_vendor("/proj/src/App.php"));
        assert!(l.is_vendor("/proj/vendor/pkg/Lib.php"));
    }

    #[test]
    fn an_empty_or_blank_extra_vendor_dir_entry_matches_nothing() {
        let l = ProjectLayout::fallback().with_extra_vendor_dirs(vec![String::new(), "/".to_owned()]);
        assert!(!l.is_vendor("anything/at/all.php"));
        assert!(l.is_vendor("vendor/pkg/Lib.php"));
    }
}
