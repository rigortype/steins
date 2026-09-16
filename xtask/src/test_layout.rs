//! One integration-test binary per crate.
//!
//! Cargo compiles every `.rs` file directly under a crate's `tests/` as its
//! own test crate, and each links the whole library: steins-infer's 171 files
//! were 171 executables of about 24 MB, most of every target dir. So a crate's
//! integration tests live under `tests/it/` as modules of `tests/it/main.rs`.
//!
//! That layout fails quietly in two ways, and these tests make both loud: a
//! new file placed directly under `tests/` brings the per-file binary back,
//! and a file under `tests/it/` that `main.rs` does not declare is never
//! compiled, so its tests never run.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn crate_dirs() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace root");
        let mut dirs: Vec<PathBuf> = fs::read_dir(root.join("crates"))
            .expect("crates/")
            .map(|e| e.expect("crates/ entry").path())
            .filter(|p| p.join("Cargo.toml").is_file())
            .collect();
        dirs.sort();
        dirs
    }

    fn rs_stems(dir: &Path) -> BTreeSet<String> {
        let Ok(entries) = fs::read_dir(dir) else { return BTreeSet::new() };
        entries
            .map(|e| e.expect("tests/ entry").path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "rs"))
            .map(|p| p.file_stem().expect("stem").to_string_lossy().into_owned())
            .collect()
    }

    /// The names `main.rs` declares with `mod name;`, attributes and a
    /// visibility on the same line allowed.
    fn declared_modules(main_rs: &str) -> BTreeSet<String> {
        main_rs
            .lines()
            .filter_map(|line| {
                let mut rest = line.trim();
                while let Some(attr) = rest.strip_prefix("#[") {
                    rest = attr.split_once(']')?.1.trim_start();
                }
                if let Some(vis) = rest.strip_prefix("pub") {
                    rest = vis.trim_start();
                    if let Some(scoped) = rest.strip_prefix('(') {
                        rest = scoped.split_once(')')?.1.trim_start();
                    }
                }
                let name = rest.strip_prefix("mod ")?.trim().strip_suffix(';')?.trim();
                Some(name.to_owned())
            })
            .collect()
    }

    #[test]
    fn no_test_file_sits_directly_under_tests() {
        let stray: Vec<String> = crate_dirs()
            .iter()
            .flat_map(|dir| {
                rs_stems(&dir.join("tests"))
                    .into_iter()
                    .map(move |stem| format!("{}/tests/{stem}.rs", dir.display()))
            })
            .collect();
        assert!(
            stray.is_empty(),
            "each of these compiles to its own test binary; move it to tests/it/ and declare it in \
             tests/it/main.rs:\n  {}",
            stray.join("\n  ")
        );
    }

    #[test]
    fn every_file_under_tests_it_is_declared() {
        let mut undeclared = Vec::new();
        for dir in crate_dirs() {
            let it = dir.join("tests/it");
            if !it.is_dir() {
                continue;
            }
            let main_rs = fs::read_to_string(it.join("main.rs"))
                .unwrap_or_else(|e| panic!("{}/main.rs: {e}", it.display()));
            let declared = declared_modules(&main_rs);
            let mut files = rs_stems(&it);
            files.remove("main");
            for stem in files.difference(&declared) {
                undeclared.push(format!("{}/{stem}.rs", it.display()));
            }
        }
        assert!(
            undeclared.is_empty(),
            "never compiled, so their tests never run; declare each with `mod <name>;` in \
             tests/it/main.rs:\n  {}",
            undeclared.join("\n  ")
        );
    }

    #[test]
    fn declared_modules_reads_attributes_and_visibility() {
        let src =
            "//! docs\n\nmod a;\n#[cfg(unix)] mod b;\npub(crate) mod c;\n// mod d;\nmod e {}\n";
        let names: Vec<String> = declared_modules(src).into_iter().collect();
        assert_eq!(names, ["a", "b", "c"]);
    }
}
