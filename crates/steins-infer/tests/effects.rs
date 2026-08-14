//! Acceptance tests for the effect-envelope check (`effect.envelope-exceeded`,
//! ADR-0005): flags a `#[\Steins\Pure]`/`#[\Steins\Effect(...)]` function whose
//! inferred effects exceed its envelope. Proven violations only; unknown
//! effects stay silent (deferred "cannot-verify").

use steins_infer::{Diagnostic, EFFECT_ID, UNKNOWN_LABEL_ID, check};
use steins_syntax::SourceTree;

/// Parse + check inline PHP, returning only the effect-envelope findings.
fn effects(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

/// Parse + check inline PHP, returning only the unknown-label findings.
fn unknown_labels(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == UNKNOWN_LABEL_ID).collect()
}

fn one(src: &str) -> Diagnostic {
    let f = effects(src);
    assert_eq!(f.len(), 1, "expected exactly one effect finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

#[test]
fn pure_calling_rand_is_flagged_with_exact_message() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction withRng(): int { return rand(); }\n";
    let d = one(src);
    assert_eq!(d.id, EFFECT_ID);
    assert_eq!(
        d.message,
        "rand() has effect nondet.random, but withRng() is declared #[\\Steins\\Pure]"
    );
    assert_eq!(d.line, 3);
}

#[test]
fn pure_builtin_and_arithmetic_are_silent() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): string { $x = 1 + 2; return strtolower($s); }\n";
    assert_eq!(effects(src).len(), 0, "pure builtin + arithmetic → silent");
}

#[test]
fn pure_calling_uncatalogued_builtin_is_silent() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { some_unknown_fn(); }\n";
    assert_eq!(effects(src).len(), 0, "uncatalogued builtin → silent (deferred)");
}

/// Issue #279: an aliased builtin import must color like the spelled call.
#[test]
fn pure_calling_an_aliased_builtin_import_is_flagged_like_the_spelled_call() {
    let src = "<?php\nuse function rand as r;\n#[\\Steins\\Pure]\nfunction withRng(): int { return r(); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "r() has effect nondet.random, but withRng() is declared #[\\Steins\\Pure]"
    );
}

#[test]
fn echo_inside_if_inside_pure_is_flagged() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(bool $c): void { if ($c) { echo \"hi\"; } }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "echo has effect io.output.buffer, but f() is declared #[\\Steins\\Pure]"
    );
}

/// Raw text between `?>` and `<?php` is output the engine writes for you
/// (ADR-0008 spec gap, wired by ADR-0083); it carries the same OB-capturable
/// label as `echo`.
#[test]
fn inline_html_inside_pure_is_flagged() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { ?><b>hi</b><?php }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "inline HTML has effect io.output.buffer, but f() is declared #[\\Steins\\Pure]"
    );
}

/// Whitespace between tag pairs is layout, not output (else indentation would color).
#[test]
fn whitespace_only_inline_text_is_not_an_output_origin() {
    for body in ["?>\n<?php", "?> <?php", "?>\n    \n<?php", "?>\t<?php"] {
        let src =
            format!("<?php\n#[\\Steins\\Pure]\nfunction f(): void {{ {body} }}\n");
        assert_eq!(effects(&src).len(), 0, "blank inline text is silent: {body:?}");
    }
}

// exit (ADR-0019 rule 4).

#[test]
fn exit_inside_pure_is_flagged() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { exit(1); }\n";
    let d = one(src);
    assert_eq!(d.message, "exit has effect exit, but f() is declared #[\\Steins\\Pure]");
}

// throw is permitted by Pure (ADR-0006).

#[test]
fn throw_inside_pure_is_silent() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { throw new \\RuntimeException(\"x\"); }\n";
    assert_eq!(effects(src).len(), 0, "Pure permits throw");
}

#[test]
fn transitive_effect_reports_via_origin() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { helper(); }\nfunction helper(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "helper() has effect io.fs.write (via file_put_contents at line 4), but f() is declared #[\\Steins\\Pure]"
    );
    assert_eq!(d.line, 3);
}

#[test]
fn transitive_through_two_hops() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { g(); }\nfunction g(): void { h(); }\nfunction h(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert!(
        d.message.contains("g() has effect io.fs.write (via file_put_contents at line 5)"),
        "got: {}",
        d.message
    );
}

#[test]
fn transitive_pure_helper_is_silent() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): void { helper($s); }\nfunction helper(string $s): string { return strtolower($s); }\n";
    assert_eq!(effects(src).len(), 0, "pure helper → silent");
}

#[test]
fn mutual_recursion_terminates() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction a(): void { b(); }\nfunction b(): void { a(); }\n";
    assert_eq!(effects(src).len(), 0);
    let effectful = "<?php\n#[\\Steins\\Pure]\nfunction a(): void { b(); }\nfunction b(): void { a(); rand(); }\n";
    let f = effects(effectful);
    assert!(
        f.iter().any(|d| d.message.contains("b() has effect nondet.random")),
        "effect through the cycle is found: {f:#?}"
    );
}

#[test]
fn bare_pure_without_use_is_not_checked() {
    // JetBrains collision guard: #[Pure] without `use Steins\Pure` is not an envelope.
    let src = "<?php\n#[Pure]\nfunction f(): int { return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "#[Pure] without use is not the Steins envelope");
}

#[test]
fn bare_pure_with_use_is_checked() {
    let src = "<?php\nuse Steins\\Pure;\n#[Pure]\nfunction f(): int { return rand(); }\n";
    let d = one(src);
    assert!(d.message.contains("rand() has effect nondet.random"), "got: {}", d.message);
}

#[test]
fn jetbrains_qualified_pure_is_not_checked() {
    let src = "<?php\n#[JetBrains\\PhpStorm\\Pure]\nfunction f(): int { return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "#[JetBrains\\PhpStorm\\Pure] is not the Steins envelope");
}

#[test]
fn effect_and_type_findings_coexist() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(int $w): int { return rand() + $w; }\nf(\"abc\");\n";
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let all = check(&tree, &functions, "test.php");
    assert!(
        all.iter().any(|d| d.id == "type.argument-mismatch"),
        "type finding present: {all:#?}"
    );
    assert!(all.iter().any(|d| d.id == EFFECT_ID), "effect finding present: {all:#?}");
}

#[test]
fn unannotated_function_with_effects_is_silent() {
    let src = "<?php\nfunction f(): int { echo \"hi\"; return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "no envelope → no effect check");
}

// ADR-0018: hierarchical `#[\Steins\Effect(...)]` envelopes — subsumption.

#[test]
fn effect_io_subsumes_io_fs_write() {
    let src = "<?php\n#[\\Steins\\Effect('io')]\nfunction f(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    assert_eq!(effects(src).len(), 0, "io subsumes io.fs.write → silent");
}

#[test]
fn effect_io_does_not_admit_nondet_random() {
    let src = "<?php\n#[\\Steins\\Effect('io')]\nfunction f(): int { return rand(); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "rand() has effect nondet.random, but f() is declared #[\\Steins\\Effect('io')] — nondet.random exceeds the envelope"
    );
    assert_eq!(d.line, 3);
}

#[test]
fn narrow_read_envelope_flags_a_write() {
    let src = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction f(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "file_put_contents() has effect io.fs.write, but f() is declared #[\\Steins\\Effect('io.fs.read')] — io.fs.write exceeds the envelope"
    );
}

#[test]
fn narrow_read_envelope_admits_a_read() {
    let src = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction f(): void { file_get_contents(\"/tmp/x\"); }\n";
    assert_eq!(effects(src).len(), 0, "io.fs.read admits io.fs.read → silent");
}

// Issue #318: a wrapper-capable builtin's argument-blind row is `io`, the parent
// of every channel a wrapper can reach; what a call site proves narrows it down.

#[test]
fn a_proven_url_target_exceeds_a_filesystem_envelope() {
    // Headline false negative fixed: `io.fs.read` used to admit any network read.
    let src = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction fetch(): string { return file_get_contents('https://example.com/rates'); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "file_get_contents() has effect io.net.http, but fetch() is declared #[\\Steins\\Effect('io.fs.read')] — io.net.http exceeds the envelope"
    );
    assert_eq!(d.line, 3);
}

#[test]
fn a_literal_local_path_stays_silent_under_the_same_envelope() {
    let src = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction load(): string { return file_get_contents('/etc/passwd'); }\n";
    assert_eq!(effects(src).len(), 0, "a proven local path is still io.fs.read → silent");
}

#[test]
fn an_unprovable_path_widens_to_the_io_default() {
    // No proof, no narrowing: `io` is the honest upper bound of a stream wrapper.
    let src = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction load(string $url): string { return file_get_contents($url); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "file_get_contents() has effect io, but load() is declared #[\\Steins\\Effect('io.fs.read')] — io exceeds the envelope"
    );
}

#[test]
fn a_resource_of_unknown_provenance_exceeds_a_filesystem_envelope() {
    let src = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction pull($r): string { return fread($r, 8); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "fread() has effect io, but pull() is declared #[\\Steins\\Effect('io.fs.read')] — io exceeds the envelope"
    );
}

#[test]
fn a_write_to_an_unknown_resource_exceeds_a_filesystem_envelope() {
    let src = "<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction push($sock): void { fwrite($sock, 'x'); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "fwrite() has effect io, but push() is declared #[\\Steins\\Effect('io.fs.write')] — io exceeds the envelope"
    );
}

#[test]
fn fwrite_to_stdout_is_the_output_channel_not_the_filesystem() {
    // ADR-0083 row awaiting argument awareness: exceeds an `io.fs.write` envelope.
    let bad = "<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction emit(): void { fwrite(STDOUT, 'x'); }\n";
    let d = one(bad);
    assert_eq!(
        d.message,
        "fwrite() has effect io.output.stdout, but emit() is declared #[\\Steins\\Effect('io.fs.write')] — io.output.stdout exceeds the envelope"
    );
    let ok = "<?php\n#[\\Steins\\Effect('io.output')]\nfunction emit(): void { fwrite(STDOUT, 'x'); }\n";
    assert_eq!(effects(ok).len(), 0, "io.output subsumes io.output.stdout → silent");
    let wide = "<?php\n#[\\Steins\\Effect('io')]\nfunction emit(): void { fwrite(STDERR, 'x'); }\n";
    assert_eq!(effects(wide).len(), 0, "io subsumes io.output.stderr → silent");
}

#[test]
fn the_php_pseudo_streams_are_read_at_the_call_site() {
    let out = "<?php\n#[\\Steins\\Pure]\nfunction emit(string $s): void { file_put_contents('php://output', $s); }\n";
    assert_eq!(
        one(out).message,
        "file_put_contents() has effect io.output.buffer, but emit() is declared #[\\Steins\\Pure]"
    );
    let inp = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction body(): string { return file_get_contents('php://input'); }\n";
    assert_eq!(
        one(inp).message,
        "file_get_contents() has effect io.input, but body() is declared #[\\Steins\\Effect('io.fs.read')] — io.input exceeds the envelope"
    );
    let filtered = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction fetch(): string { return file_get_contents('php://filter/read=convert.base64-encode/resource=https://example.com/r'); }\n";
    assert_eq!(
        one(filtered).message,
        "file_get_contents() has effect io.net.http, but fetch() is declared #[\\Steins\\Effect('io.fs.read')] — io.net.http exceeds the envelope"
    );
    // data:// reaches no channel; mutate.local is tolerated by every envelope (ADR-0063).
    let data = "<?php\n#[\\Steins\\Pure]\nfunction inline(): string { return file_get_contents('data://text/plain,hi'); }\n";
    assert_eq!(effects(data).len(), 0, "a data URI is mutate.local → tolerated even by Pure");
    // Unknown scheme keeps `io` (ruling D-W1: userland wrappers are approximated).
    let unknown = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction fetch(): string { return file_get_contents('acme://bucket/key'); }\n";
    assert_eq!(
        one(unknown).message,
        "file_get_contents() has effect io, but fetch() is declared #[\\Steins\\Effect('io.fs.read')] — io exceeds the envelope"
    );
}

#[test]
fn fopen_composes_its_direction_from_a_literal_mode() {
    // Read mode under a read envelope: silent (old parent `io.fs` row would report).
    let read = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction open(): mixed { return fopen('/tmp/x', 'r'); }\n";
    assert_eq!(effects(read).len(), 0, "a proven 'r' mode on a local path is io.fs.read");
    let write = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction open(): mixed { return fopen('/tmp/x', 'w'); }\n";
    assert_eq!(
        one(write).message,
        "fopen() has effect io.fs.write, but open() is declared #[\\Steins\\Effect('io.fs.read')] — io.fs.write exceeds the envelope"
    );
    let dynamic = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction open(string $m): mixed { return fopen('/tmp/x', $m); }\n";
    assert_eq!(
        one(dynamic).message,
        "fopen() has effect io.fs, but open() is declared #[\\Steins\\Effect('io.fs.read')] — io.fs exceeds the envelope"
    );
}

#[test]
fn a_two_target_row_reads_each_side_in_its_own_role() {
    let both = "<?php\n#[\\Steins\\Effect('io.fs')]\nfunction dup(): bool { return copy('/a', '/b'); }\n";
    assert_eq!(effects(both).len(), 0, "io.fs subsumes both halves");
    let write_only = "<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction dup(): bool { return copy('/a', '/b'); }\n";
    assert_eq!(
        one(write_only).message,
        "copy() has effect io.fs.read, but dup() is declared #[\\Steins\\Effect('io.fs.write')] — io.fs.read exceeds the envelope"
    );
    // rename is a metadata write on both sides, no contents read.
    let mv = "<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction mv(): bool { return rename('/a', '/b'); }\n";
    assert_eq!(effects(mv).len(), 0, "rename writes on both sides → io.fs.write alone");
    let remote = "<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction dup(): bool { return copy('https://h/a', '/b'); }\n";
    let f = effects(remote);
    assert_eq!(f.len(), 1, "only the network half exceeds, got: {f:#?}");
    assert_eq!(
        f[0].message,
        "copy() has effect io.net.http, but dup() is declared #[\\Steins\\Effect('io.fs.write')] — io.net.http exceeds the envelope"
    );
    // One unprovable side: no narrowing rather than faked precision → `io`.
    let half = "<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction dup(string $to): bool { return copy('/a', $to); }\n";
    assert_eq!(
        one(half).message,
        "copy() has effect io, but dup() is declared #[\\Steins\\Effect('io.fs.write')] — io exceeds the envelope"
    );
}

#[test]
fn the_stat_and_unlink_family_is_wrapper_capable_too() {
    let local = "<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction wipe(): void { unlink('/tmp/x'); }\n";
    assert_eq!(effects(local).len(), 0, "a proven local path is still io.fs.write");
    let stat = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction has(): bool { return file_exists('/tmp/x'); }\n";
    assert_eq!(effects(stat).len(), 0, "a proven local path is still io.fs.read");
    // Over a wrapper the same call is a network round trip — why the row widened.
    let remote = "<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction wipe(): void { unlink('ssh2.sftp://h/x'); }\n";
    assert_eq!(
        one(remote).message,
        "unlink() has effect io.net, but wipe() is declared #[\\Steins\\Effect('io.fs.write')] — io.net exceeds the envelope"
    );
    let remote_stat = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction has(): bool { return file_exists('ftp://h/x'); }\n";
    assert_eq!(
        one(remote_stat).message,
        "file_exists() has effect io.net, but has() is declared #[\\Steins\\Effect('io.fs.read')] — io.net exceeds the envelope"
    );
    let dynamic = "<?php\n#[\\Steins\\Effect('io.fs.write')]\nfunction wipe(string $p): void { unlink($p); }\n";
    assert_eq!(
        one(dynamic).message,
        "unlink() has effect io, but wipe() is declared #[\\Steins\\Effect('io.fs.write')] — io exceeds the envelope"
    );
    // No stream opened: php:// names no channel here, so `io` stands.
    let pseudo = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction has(): bool { return is_file('php://stdout'); }\n";
    assert_eq!(
        one(pseudo).message,
        "is_file() has effect io, but has() is declared #[\\Steins\\Effect('io.fs.read')] — io exceeds the envelope"
    );
}

#[test]
fn the_read_and_relay_pair_keeps_its_output_component_when_narrowed() {
    // readfile is both halves once proven: read-only envelope still catches output (ADR-0083).
    let src = "<?php\n#[\\Steins\\Effect('io.fs.read')]\nfunction serve(): void { readfile('/var/www/x'); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "readfile() has effect io.output.buffer, but serve() is declared #[\\Steins\\Effect('io.fs.read')] — io.output.buffer exceeds the envelope"
    );
    let wide = "<?php\n#[\\Steins\\Effect('io')]\nfunction serve(): void { readfile('/var/www/x'); }\n";
    assert_eq!(effects(wide).len(), 0, "io subsumes both components → silent");
}

#[test]
fn the_process_global_rows_are_proven_effects_not_silent_taint() {
    for (call, name) in
        [("srand(1)", "srand"), ("mt_srand(1)", "mt_srand"), ("clearstatcache()", "clearstatcache")]
    {
        let src = format!("<?php\n#[\\Steins\\Pure]\nfunction f(): void {{ {call}; }}\n");
        assert_eq!(
            one(&src).message,
            format!("{name}() has effect global.write, but f() is declared #[\\Steins\\Pure]")
        );
    }
    // Seeding and drawing are different effects: global.write admits only the write.
    let seeded = "<?php\n#[\\Steins\\Effect('global.write')]\nfunction reseed(): void { mt_srand(1); }\n";
    assert_eq!(effects(seeded).len(), 0, "global.write admits the seeding row → silent");
    let drawn = "<?php\n#[\\Steins\\Effect('global.write')]\nfunction draw(): int { return mt_rand(); }\n";
    assert_eq!(
        one(drawn).message,
        "mt_rand() has effect nondet.random, but draw() is declared #[\\Steins\\Effect('global.write')] — nondet.random exceeds the envelope"
    );
}

#[test]
fn nondet_envelope_covers_random_and_time() {
    let src = "<?php\n#[\\Steins\\Effect('nondet')]\nfunction f(): int { return rand() + time(); }\n";
    assert_eq!(effects(src).len(), 0, "nondet subsumes both nondet.random and nondet.time");
}

#[test]
fn multi_label_envelope_admits_each_subtree() {
    let ok = "<?php\n#[\\Steins\\Effect('io', 'nondet.time')]\nfunction f(): void { file_put_contents(\"/x\", \"y\"); time(); }\n";
    assert_eq!(effects(ok).len(), 0, "both effects subsumed → silent");

    let bad = "<?php\n#[\\Steins\\Effect('io', 'nondet.time')]\nfunction f(): int { return rand(); }\n";
    let d = one(bad);
    assert_eq!(
        d.message,
        "rand() has effect nondet.random, but f() is declared #[\\Steins\\Effect('io', 'nondet.time')] — nondet.random exceeds the envelope"
    );
}

#[test]
fn effect_exit_admits_exit_but_pure_forbids_it() {
    // ADR-0019: #[Effect('exit')] permits exit; Pure still forbids it.
    let permitted = "<?php\n#[\\Steins\\Effect('exit')]\nfunction f(): void { exit(1); }\n";
    assert_eq!(effects(permitted).len(), 0, "Effect('exit') admits exit → silent");

    let forbidden = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { exit(1); }\n";
    let d = one(forbidden);
    assert_eq!(d.message, "exit has effect exit, but f() is declared #[\\Steins\\Pure]");
}

#[test]
fn effect_io_output_admits_echo() {
    let src =
        "<?php\n#[\\Steins\\Effect('io.output')]\nfunction f(): void { echo \"hi\"; }\n";
    assert_eq!(effects(src).len(), 0, "Effect('io.output') admits echo → silent");
}

/// ADR-0083's one deliberate meaning change: output is now ambient under `io`,
/// so a bare `io` envelope admits `echo` (intended; "io but no output" needs
/// enumerated children, or #312's `io -except io.output`).
#[test]
fn a_bare_io_envelope_admits_echo() {
    let src = "<?php\n#[\\Steins\\Effect('io')]\nfunction f(): void { echo \"hi\"; }\n";
    assert_eq!(effects(src).len(), 0, "io.output.buffer ⊑ io → silent");
}

/// A fine-grained envelope keeps its edge: `io.db` doesn't subsume output, so
/// echo is still a violation — the migration blunted only bare `io`.
#[test]
fn a_fine_grained_io_envelope_still_catches_echo() {
    let src = "<?php\n#[\\Steins\\Effect('io.db')]\nfunction f(): void { echo \"hi\"; }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "echo has effect io.output.buffer, but f() is declared #[\\Steins\\Effect('io.db')] \
         — io.output.buffer exceeds the envelope"
    );
}

/// `io.output.buffer` is the OB-capturable leaf only: `header()` is response
/// metadata `ob_start()` cannot touch, so it exceeds that envelope alone.
#[test]
fn the_buffer_leaf_admits_echo_but_not_a_header_call() {
    let admits = "<?php\n#[\\Steins\\Effect('io.output.buffer')]\nfunction f(): void { echo \"hi\"; }\n";
    assert_eq!(effects(admits).len(), 0, "echo is OB-capturable output");
    let exceeds = "<?php\n#[\\Steins\\Effect('io.output.buffer')]\nfunction f(): void { header('X: 1'); }\n";
    let d = one(exceeds);
    assert_eq!(
        d.message,
        "header() has effect io.output.header, but f() is declared \
         #[\\Steins\\Effect('io.output.buffer')] — io.output.header exceeds the envelope"
    );
    let parent = "<?php\n#[\\Steins\\Effect('io.output')]\nfunction f(): void { header('X: 1'); }\n";
    assert_eq!(effects(parent).len(), 0, "the umbrella admits response metadata too");
}

/// `output` → `io.output` is Levenshtein 3, past the suggestion cap, so no
/// "did you mean" reaches it; the retirement table (issue #311) spells out
/// the ADR-0083 migration instead.
#[test]
fn the_retired_output_spelling_is_an_unknown_label_carrying_its_migration() {
    let src = "<?php\n#[\\Steins\\Effect('output')]\nfunction f(): void { echo \"hi\"; }\n";
    let u = unknown_labels(src);
    assert_eq!(u.len(), 1, "{u:#?}");
    assert_eq!(u[0].id, UNKNOWN_LABEL_ID, "same id, same layer, same floor as it always had");
    assert_eq!(
        u[0].message,
        "unknown effect label 'output' in #[\\Steins\\Effect] on f() — 'output' was retired, so \
         write io.output.buffer for echo-shaped code, io.output.header for header()/setcookie(), \
         or the umbrella io.output"
    );
    // The unknown label still reads as an envelope, so echo is also reported.
    let d = one(src);
    assert_eq!(
        d.message,
        "echo has effect io.output.buffer, but f() is declared #[\\Steins\\Effect('output')] \
         — io.output.buffer exceeds the envelope"
    );
}

#[test]
fn non_literal_effect_args_impose_no_checking() {
    // A class-constant argument makes the attribute unrecognized.
    let src = "<?php\n#[\\Steins\\Effect(Effects::IO)]\nfunction f(): int { return rand(); }\n";
    assert_eq!(effects(src).len(), 0, "unrecognized attribute → no envelope check");
    assert_eq!(unknown_labels(src).len(), 0, "unrecognized attribute → no unknown-label");
}

#[test]
fn transitive_effect_exceeds_declared_envelope() {
    let src = "<?php\n#[\\Steins\\Effect('nondet')]\nfunction loadCfg(): void { helper(); }\nfunction helper(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "helper() has effect io.fs.write (via file_put_contents at line 4), but loadCfg() is declared #[\\Steins\\Effect('nondet')] — io.fs.write exceeds the envelope"
    );
    assert_eq!(d.line, 3, "reported at the outer helper() call site");
}

#[test]
fn transitive_effect_within_envelope_is_silent() {
    let src = "<?php\n#[\\Steins\\Effect('io')]\nfunction f(): void { helper(); }\nfunction helper(): void { file_put_contents(\"/tmp/x\", \"y\"); }\n";
    assert_eq!(effects(src).len(), 0, "transitive io.fs.write under io → silent");
}

// ADR-0018: unknown-label registry diagnostic.

#[test]
fn typo_label_reports_unknown_with_suggestion() {
    let src = "<?php\n#[\\Steins\\Effect('io.netw')]\nfunction f(): void {}\n";
    let u = unknown_labels(src);
    assert_eq!(u.len(), 1, "one unknown-label finding: {u:#?}");
    assert_eq!(u[0].id, UNKNOWN_LABEL_ID);
    assert_eq!(
        u[0].message,
        "unknown effect label 'io.netw' in #[\\Steins\\Effect] on f() — did you mean 'io.net'?"
    );
    assert_eq!(u[0].line, 2);
}

#[test]
fn private_label_is_unknown_for_now() {
    // email.send is a semantic/plugin label the registry does not yet know.
    let src = "<?php\n#[\\Steins\\Effect('email.send')]\nfunction f(): void {}\n";
    let u = unknown_labels(src);
    assert_eq!(u.len(), 1, "email.send is unknown until plugins can register it");
    assert!(u[0].message.contains("unknown effect label 'email.send'"), "got: {}", u[0].message);
}

#[test]
fn registry_roots_produce_no_unknown_label() {
    for label in [
        "io.output", "io.output.buffer", "io.output.header", "io.output.stdout",
        "io.output.stderr", "io.input", "io", "io.fs", "io.fs.read", "io.fs.write", "io.net",
        "io.net.http", "io.db", "io.process", "global.read", "global.write", "nondet",
        "nondet.random", "nondet.time", "exit", "mutate",
    ] {
        let src = format!("<?php\n#[\\Steins\\Effect('{label}')]\nfunction f(): void {{}}\n");
        assert_eq!(unknown_labels(&src).len(), 0, "{label} is a known registry root");
    }
}

#[test]
fn pure_never_produces_unknown_label() {
    // Pure has an empty label set → no label can be unknown.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void {}\n";
    assert_eq!(unknown_labels(src).len(), 0);
}

#[test]
fn method_effect_envelope_exceeded_via_this_helper() {
    let src = "<?php\nfinal class Svc {\n  #[\\Steins\\Effect('io')]\n  public function run(): void { $this->helper(); }\n  private function helper(): void { rand(); }\n}\n";
    let d = one(src);
    assert_eq!(
        d.message,
        "Svc::helper() has effect nondet.random (via rand at line 5), but Svc::run() is declared #[\\Steins\\Effect('io')] — nondet.random exceeds the envelope"
    );
    assert_eq!(d.line, 4);
}

#[test]
fn method_effect_envelope_admits_subsumed_helper_effect() {
    let src = "<?php\nfinal class Svc {\n  #[\\Steins\\Effect('io')]\n  public function run(): void { $this->helper(); }\n  private function helper(): void { file_put_contents(\"/x\", \"y\"); }\n}\n";
    assert_eq!(effects(src).len(), 0, "io.fs.write under io → silent");
}
