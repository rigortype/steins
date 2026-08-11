//! End-to-end tests for `steins version` and `steins license` (issue #43).
//!
//! Both carry a legal obligation rather than a convenience: the release archive
//! ships `LICENSE` and `THIRD-PARTY-LICENSES.md` beside the binary, but nothing
//! downstream keeps them together — the Homebrew formula installs the executable
//! and the third-party notices and *not* `LICENSE`, and `cargo install --git`
//! produces a bare binary. So the notices are embedded, and these tests check
//! that what is embedded is the real thing rather than a stub.

use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

/// Every test in this file spawns the binary with `GITHUB_ACTIONS` scrubbed.
/// `check`'s format auto-detection (ADR-0054 §6) reads that variable, so a test
/// run *on* GitHub Actions would otherwise get workflow commands where it
/// asserted text. No test's expected output may depend on the ambient CI
/// environment; detection itself is tested in `tests/format_github.rs`, which
/// sets the variable deliberately.
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

    // The dependencies' notices, whose MIT/BSD/ISC terms require them to
    // accompany a binary distribution.
    assert!(out.contains("Third-Party Licenses"));
    assert!(out.contains("Permission is hereby granted"), "an MIT body must be present");

    let (code2, out2) = run(&["licenses"]);
    assert_eq!(code2, 0);
    assert_eq!(out, out2);
}

#[test]
fn license_carries_phpstans_notice() {
    // PHPStan is Steins' direct model (README "Acknowledgments") but is not a
    // Rust dependency `cargo xtask licenses` can discover, so
    // `THIRD-PARTY-LICENSES.md` alone would never carry its notice — this has
    // to be guaranteed some other way, which is why it is embedded directly
    // (see `LICENSE_PHPSTAN` in `main.rs`).
    let (code, out) = run(&["license"]);
    assert_eq!(code, 0);
    assert!(out.contains("PHPStan"), "must credit PHPStan as the direct model");
    assert!(out.contains("Copyright (c) 2016 Ondřej Mirtes"), "PHPStan's own copyright notice is missing");
    assert!(out.contains("Copyright (c) 2025 PHPStan s.r.o."), "PHPStan s.r.o.'s copyright notice is missing");
    // PHPStan's own MIT body must appear ahead of the third-party dependency
    // notices, not merely somewhere among the 39 other MIT-licensed crates.
    let before_third_party = out.split("Third-Party Licenses").next().expect("a prefix");
    assert!(
        before_third_party.contains("Copyright (c) 2016 Ondřej Mirtes"),
        "PHPStan's notice must stand on its own, ahead of the dependency notices"
    );
}

#[test]
fn the_embedded_notices_are_generated_not_a_stub() {
    // Guards the failure this whole surface exists to prevent: a placeholder or
    // truncated notices file that makes the command look compliant while
    // carrying nothing. Assert real bodies and a real dependency, never merely
    // that the output is non-empty.
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
    // cargo-about groups on exact license text, so Apache-2.0 shipped centred by
    // one crate and flush-left by another produces two sections of the same
    // name — left that way deliberately (matching rigortype/lisplens's
    // about.toml/about.hbs, which carries no merge pass either): one crate, one
    // block, is easier to scan than a merged notice, and every crate's own
    // copyright line stays paired with its own license body rather than pooled
    // above a shared one.
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
    // Each MIT-licensed crate keeps its own section — no permission-notice
    // merge collapses distinct copyright holders into one block. The embedded
    // copy is what a `brew install` user reads, so the property is checked on
    // the command's output and not only on the file.
    let (_, out) = run(&["license"]);
    let third_party = out.split("Third-Party Licenses").nth(1).expect("third-party section");
    let mit_sections = third_party.matches("\n## MIT License").count();
    assert!(mit_sections >= 30, "expected dozens of separate MIT sections, found {mit_sections}");
    // Each section carries exactly one permission notice — nothing pools
    // several crates' grants under one heading.
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
    // `steins license` emits thousands of lines, so `| head` and quitting `less`
    // early are the normal ways to read it. `println!` panics on EPIPE; this
    // pins that the command does not.
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
