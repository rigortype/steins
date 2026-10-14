//! Version-aware files: a backstop for the arity families that judge a call from the
//! analysing PHP's reflection.
//!
//! Those families run only under an exact-minor target (`Folder::arity_target_exact`),
//! where every targeted PHP is the engine and a version constant decides most guards:
//! the walk prunes `if (PHP_VERSION_ID < 80000) { mktime(); }` as dead. What it cannot
//! see through is indirection, and a verdict about code that runs on another PHP is a
//! false positive (each shape below runs clean on PHP 8.5 and 7.4):
//!
//! | shape                                                 | why the walk misses it     |
//! |-------------------------------------------------------|----------------------------|
//! | `$old = PHP_VERSION_ID < 80000; if ($old) { mktime(); }` | a stored flag           |
//! | `function isLegacy() { return PHP_VERSION_ID < 80000; }` | a helper's return       |
//! | `const LEGACY = PHP_VERSION_ID < 80000;`              | a class constant           |
//! | `while (PHP_VERSION_ID < 80000) { mktime(); }`        | a loop condition           |
//! | `if (PHP_VERSION_ID >= 80000) { return; }` then code  | an early exit              |
//! | `PHP_VERSION_ID < 80000 ? mktime() : time()`, `&&`, `||`, `and`, `or`, `?:`, `match`, `switch` | expression level |
//! | a file `require`d only behind a version check         | the file itself            |
//!
//! No region model catches them all, so the rule is one level coarser and sound by
//! construction: **a file that reads the version at all is not judged by these
//! families.** A read is `PHP_VERSION`, `PHP_VERSION_ID`, `PHP_MAJOR_VERSION`,
//! `PHP_MINOR_VERSION`, `PHP_RELEASE_VERSION`, `version_compare` or `phpversion`
//! anywhere in the file's text (a mention in a comment costs only a missed finding).
//! The cost is the compat-aware files going quiet; the gap that remains is a flag or
//! helper defined in ANOTHER file (`isLegacy()` imported), which only a per-minor arity
//! table closes.

use crate::cx::Cx;

const VERSION_READS: [&str; 6] = [
    "PHP_VERSION",
    "PHP_MAJOR_VERSION",
    "PHP_MINOR_VERSION",
    "PHP_RELEASE_VERSION",
    "version_compare",
    "phpversion",
];

/// Whether the current file reads the PHP version anywhere.
pub(crate) fn file_reads_php_version(cx: &Cx) -> bool {
    let text = cx.tree().source_from(0).unwrap_or_default();
    VERSION_READS.iter().any(|t| text.contains(t))
}
