//! The perf evidence for the per-package artifact codec decision (issue
//! #487): serialize+write and open+read-one-file wall times and artifact
//! sizes over a synthetic package, next to a cold parse of the same file for
//! scale. Self-contained — the corpus is absent in agent worktrees — and
//! ignored by default: numbers from a shared CI runner would mislead, so it
//! runs by hand (`cargo test -p steins-db --release --test persist_perf --
//! --ignored --nocapture`, and the raw prints are also why it lives outside
//! `src/`, which the output-seam scan covers).
#![cfg(feature = "persist")]

use std::path::PathBuf;

use steins_db::PackageShard;
use steins_db::persist::{TraceFile, TraceIndex, build_sections, decl_contracts, read_shard};
use steins_gen::{ArtifactReader, DecodeBudget};
use steins_syntax::SourceTree;

/// A throwaway directory under the OS temp dir, cleaned on drop.
struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-db-persist-perf-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn synth_php(i: usize) -> String {
    let mut src = format!(
        "<?php\nnamespace Synth\\P{i};\nuse Synth\\Shared\\Helper;\n\nconst LIMIT_{i} = {i};\n"
    );
    for f in 0..6 {
        src.push_str(&format!(
            "/**\n * @param int $n the count\n * @param string $tag\n * @return string\n */\nfunction fn_{i}_{f}(int $n, string $tag = 'd'): string {{\n  $acc = [];\n  $w = 0.5;\n  if ($n === {f}) {{ $acc['k'] = $tag; }} elseif ($n > 10) {{ $acc[] = 'big'; }} else {{ $acc[] = 'small'; }}\n  foreach ($acc as $k => $v) {{ $out[] = $v . '-{f}'; }}\n  $c = fn (int $x): int => $x + {f};\n  echo $tag;\n  return implode(',', $out ?? []) . $c($n) . $w;\n}}\n"
        ));
    }
    src.push_str(&format!(
        "/** @method int magic() */\nclass C{i} {{\n  public function m(int $a, string $b = 'x'): string {{ return $b . $a; }}\n  /** @return self */\n  public static function make(): self {{ return new self(); }}\n}}\n"
    ));
    src
}

#[test]
#[ignore = "perf evidence for the PR body, run by hand with --release and --nocapture"]
fn perf_evidence() {
    let n_files = 200;
    let sources: Vec<(String, String)> =
        (0..n_files).map(|i| (format!("vendor/synth/pkg/src/f{i}.php"), synth_php(i))).collect();

    let t = std::time::Instant::now();
    let parsed: Vec<(String, SourceTree)> =
        sources.iter().map(|(p, s)| (p.clone(), SourceTree::parse(s))).collect();
    let parse_all = t.elapsed();

    let t = std::time::Instant::now();
    let mut shard = PackageShard::default();
    let mut contracts = Vec::new();
    for (slot, (path, tree)) in parsed.iter().enumerate() {
        shard.add_file(slot, path, tree);
        contracts.extend(decl_contracts(slot, tree));
    }
    let build_tables = t.elapsed();

    let trace: Vec<TraceFile<'_>> = parsed
        .iter()
        .enumerate()
        .map(|(slot, (path, tree))| TraceFile { path, slot, tree })
        .collect();
    let tmp = TempDir::new("perf");
    let path = tmp.dir.join("synth.pkg");
    let t = std::time::Instant::now();
    build_sections(&shard, &contracts, &trace).write_to(&path).unwrap();
    let serialize_write = t.elapsed();
    let artifact_len = std::fs::metadata(&path).unwrap().len();
    let src_len: usize = sources.iter().map(|(_, s)| s.len()).sum();

    let open = |path: &std::path::Path| ArtifactReader::open(path, DecodeBudget::default()).unwrap();

    let probe = &parsed[n_files / 2].0;
    let t = std::time::Instant::now();
    let mut reader = open(&path);
    let index = TraceIndex::open(&mut reader).unwrap();
    let one = index.read_tree(&mut reader, probe).unwrap();
    let open_read_one = t.elapsed();
    assert_eq!(&one, &parsed[n_files / 2].1);

    let t = std::time::Instant::now();
    let mut reader = open(&path);
    let decoded = read_shard(&mut reader).unwrap();
    let open_read_shard = t.elapsed();
    assert_eq!(decoded, shard);

    let t = std::time::Instant::now();
    let reparsed = SourceTree::parse(&sources[n_files / 2].1);
    let parse_one = t.elapsed();
    assert_eq!(reparsed, parsed[n_files / 2].1);

    let t = std::time::Instant::now();
    let mut reader = open(&path);
    let index = TraceIndex::open(&mut reader).unwrap();
    for (path, _) in &parsed {
        index.read_tree(&mut reader, path).unwrap();
    }
    let read_all = t.elapsed();

    eprintln!("files: {n_files}, source bytes: {src_len}");
    eprintln!("parse all (cold fixture prep):  {parse_all:?}");
    eprintln!("shard+contract tables:          {build_tables:?}");
    eprintln!("serialize + write + fsync:      {serialize_write:?}");
    eprintln!("artifact size:                  {artifact_len} bytes");
    eprintln!("open + read one file's tree:    {open_read_one:?}");
    eprintln!("open + read shard (symbols):    {open_read_shard:?}");
    eprintln!("read every file's tree:         {read_all:?}");
    eprintln!("cold parse of that one file:    {parse_one:?}");
}
