//! End-to-end tests for `steins version` and `steins license` (issue #43).
//!
//! Legal obligation, not convenience: the release archive ships `LICENSE` and
//! `THIRD-PARTY-LICENSES.md` beside the binary, but nothing downstream keeps them
//! together (Homebrew installs the executable + third-party notices but not
//! `LICENSE`; `cargo install --git` produces a bare binary) — so the notices are
//! embedded, and these tests check the embedded copy is real, not a stub.

use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Spawns with `GITHUB_ACTIONS` scrubbed so `check`'s CI auto-detection
/// (ADR-0054 §6) doesn't emit workflow commands where a test expects text.
/// Detection itself is tested in `tests/format_github.rs`.
fn steins_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_remove("GITHUB_ACTIONS");
    cmd
}

fn run(args: &[&str]) -> (i32, String) {
    let out = steins_cmd().args(args).output().expect("run steins");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn version_names_the_build_and_points_at_the_license_command() {
    for form in [["version"], ["--version"], ["-v"]] {
        let (code, out) = run(&form);
        assert_eq!(code, 0, "`steins {}` must succeed", form[0]);
        assert!(out.starts_with("steins "), "{form:?}: {out}");
        assert!(out.contains(env!("CARGO_PKG_VERSION")), "{form:?}");
        assert!(out.contains("revision "), "the build stamp is missing: {out}");
        assert!(out.contains("Copyright (c) TypedDuck"), "the notice must name the holder README states");
        assert!(out.contains("steins license"), "must point at the license command");
    }
}

#[test]
fn license_carries_our_terms_and_the_bundled_notices() {
    let (code, out) = run(&["license"]);
    assert_eq!(code, 0);

    // Steins' own terms — Apache-2.0 §4(a) requires a recipient to get a copy.
    assert!(out.contains("Apache License"), "our own license text is missing");
    assert!(out.contains("TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION"));

    // Dependency notices — MIT/BSD/ISC terms require them alongside the binary.
    assert!(out.contains("Third-Party Licenses"));
    assert!(out.contains("Permission is hereby granted"), "an MIT body must be present");

    let (code2, out2) = run(&["licenses"]);
    assert_eq!(code2, 0);
    assert_eq!(out, out2);
}

#[test]
fn license_carries_phpstans_notice() {
    // PHPStan is Steins' direct model (README "Acknowledgments") but isn't a Rust
    // dependency `cargo xtask licenses` can discover, so it's embedded directly
    // (`LICENSE_PHPSTAN` in `main.rs`) rather than relying on THIRD-PARTY-LICENSES.md.
    let (code, out) = run(&["license"]);
    assert_eq!(code, 0);
    assert!(out.contains("PHPStan"), "must credit PHPStan as the direct model");
    assert!(out.contains("Copyright (c) 2016 Ondřej Mirtes"), "PHPStan's own copyright notice is missing");
    assert!(out.contains("Copyright (c) 2025 PHPStan s.r.o."), "PHPStan s.r.o.'s copyright notice is missing");
    // Must lead the dependency notices, not merely appear among the 39 other MIT crates.
    let before_third_party = out.split("Third-Party Licenses").next().expect("a prefix");
    assert!(
        before_third_party.contains("Copyright (c) 2016 Ondřej Mirtes"),
        "PHPStan's notice must stand on its own, ahead of the dependency notices"
    );
}

#[test]
fn the_embedded_notices_are_generated_not_a_stub() {
    // Guards the failure this surface exists to prevent: a placeholder/truncated
    // notices file that looks compliant while carrying nothing.
    let (_, out) = run(&["license"]);
    assert!(out.matches("\n## ").count() >= 30, "expected the licence sections");
    assert!(out.contains("Used by:"));
    assert!(out.contains("— https://"), "entries must carry crate repositories");
    let notices = out.lines().filter(|l| l.starts_with("Copyright")).count();
    assert!(notices >= 30, "the bundled copyright notices are missing (found {notices})");
    for holder in ["Copyright (c) 2014 Alex Crichton", "Copyright (c) 2010 The Rust Project Developers"] {
        assert!(out.contains(holder), "missing `{holder}`");
    }
}

#[test]
fn typographic_variants_stay_in_their_own_sections() {
    // cargo-about groups on exact license text, so Apache-2.0 shipped centred by one
    // crate and flush-left by another produces two sections of the same name — left
    // that way deliberately (matches rigortype/lisplens's about.toml/about.hbs, no
    // merge pass): one block per crate keeps each copyright line paired with its body.
    let (_, out) = run(&["license"]);
    let third_party = out.split("Third-Party Licenses").nth(1).expect("third-party section");
    assert_eq!(
        third_party.matches("\n## Apache License 2.0").count(),
        2,
        "blake3 and codespan-reporting ship differently-wrapped Apache-2.0 text and must stay apart"
    );
}

#[test]
fn mit_is_one_section_per_crates_license_text() {
    // No permission-notice merge collapses distinct copyright holders into one
    // block; checked on the command's output (what `brew install` users read).
    let (_, out) = run(&["license"]);
    let third_party = out.split("Third-Party Licenses").nth(1).expect("third-party section");
    let mit_sections = third_party.matches("\n## MIT License").count();
    assert!(mit_sections >= 30, "expected dozens of separate MIT sections, found {mit_sections}");
    for section in third_party.split("\n## MIT License").skip(1) {
        let body = section.split("\n## ").next().unwrap_or(section);
        assert_eq!(
            body.matches("Permission is hereby granted").count(),
            1,
            "a section must carry exactly one grant:\n{body}"
        );
    }
}

#[test]
fn a_closed_pipe_is_not_a_crash() {
    // `steins license` emits thousands of lines, so `| head`/early-quit `less` are
    // normal; `println!` panics on EPIPE — this pins that the command does not.
    let mut child = steins_cmd()
        .arg("license")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn steins");
    // Drop the read end immediately, then wait: the writer sees EPIPE.
    drop(child.stdout.take());
    let out = child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "closed pipe must not panic: {stderr}");
    assert!(out.status.success(), "closed pipe must still exit 0");
}
