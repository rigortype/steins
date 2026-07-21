//! ADR-0003 spike: evaluate mago-syntax as the parser backend behind Steins'
//! syntax tree contract.
//!
//! Test A (structural losslessness): do leaf-node spans + trivia spans tile
//!   the whole file (no gaps, no overlaps)? `source_text` being retained is
//!   not enough for rewriting; the *tree* must account for every byte.
//! Test B (real corpus): parse success rate over real vendor code.
//! Test C (error tolerance): mutate valid files (truncate / delete brace /
//!   insert garbage) and measure how much of the tree survives and whether
//!   spans still tile the prefix.

use std::path::PathBuf;

use mago_allocator::LocalArena;
use mago_database::file::FileId;
use mago_span::HasSpan;
use mago_syntax::cst::{Node, Program};
use mago_syntax::parser::parse_file_content;
use walkdir::WalkDir;

#[derive(Default)]
struct Tiling {
    covered: Vec<bool>,
    overlaps: usize,
}

impl Tiling {
    fn new(len: usize) -> Self {
        Self { covered: vec![false; len], overlaps: 0 }
    }
    fn mark(&mut self, start: usize, end: usize) {
        for i in start..end.min(self.covered.len()) {
            if self.covered[i] {
                self.overlaps += 1;
            }
            self.covered[i] = true;
        }
    }
    fn gaps(&self) -> Vec<(usize, usize)> {
        let mut gaps = vec![];
        let mut i = 0;
        while i < self.covered.len() {
            if !self.covered[i] {
                let start = i;
                while i < self.covered.len() && !self.covered[i] {
                    i += 1;
                }
                gaps.push((start, i));
            } else {
                i += 1;
            }
        }
        gaps
    }
}

fn collect_leaf_spans(node: &Node<'_, '_>, out: &mut Vec<(usize, usize)>) {
    let children = node.children();
    if children.is_empty() {
        let span = node.span();
        out.push((span.start.offset as usize, span.end.offset as usize));
    } else {
        for child in &children {
            collect_leaf_spans(child, out);
        }
    }
}

struct FileReport {
    parse_errors: usize,
    gaps: Vec<(usize, usize)>,
    overlaps: usize,
    node_count: usize,
}

fn analyze(program: &Program<'_>, len: usize) -> FileReport {
    let mut leaf_spans = vec![];
    let root = Node::Program(program);
    collect_leaf_spans(&root, &mut leaf_spans);

    let mut node_count = 0usize;
    fn count(node: &Node<'_, '_>, acc: &mut usize) {
        *acc += 1;
        for child in &node.children() {
            count(child, acc);
        }
    }
    count(&root, &mut node_count);

    let mut tiling = Tiling::new(len);
    for &(start, end) in &leaf_spans {
        tiling.mark(start, end);
    }
    for trivia in program.trivia.iter() {
        let span = trivia.span();
        tiling.mark(span.start.offset as usize, span.end.offset as usize);
    }

    FileReport {
        parse_errors: program.errors.len(),
        gaps: tiling.gaps(),
        overlaps: tiling.overlaps,
        node_count,
    }
}

fn parse_and_analyze(content: &[u8], name: &str) -> FileReport {
    let arena = LocalArena::new();
    let file_id = FileId::new(name.as_bytes());
    let program = parse_file_content(&arena, file_id, content);
    analyze(program, content.len())
}

fn inspect(path: &str) {
    let content = std::fs::read(path).expect("read file");
    let arena = LocalArena::new();
    let file_id = FileId::new(path.as_bytes());
    let program = parse_file_content(&arena, file_id, &content);

    let mut spans: Vec<(usize, usize, String)> = vec![];
    fn walk(node: &Node<'_, '_>, out: &mut Vec<(usize, usize, String)>) {
        let children = node.children();
        if children.is_empty() {
            let span = node.span();
            out.push((span.start.offset as usize, span.end.offset as usize, format!("{:?}", node.kind())));
        } else {
            for child in &children {
                walk(child, out);
            }
        }
    }
    walk(&Node::Program(program), &mut spans);
    for trivia in program.trivia.iter() {
        let span = trivia.span();
        spans.push((span.start.offset as usize, span.end.offset as usize, format!("Trivia::{}", trivia.kind)));
    }
    spans.sort();
    let mut prev: Option<&(usize, usize, String)> = None;
    for entry in &spans {
        if let Some(p) = prev
            && p.1 > entry.0
        {
            let text = |s: usize, e: usize| String::from_utf8_lossy(&content[s..e.min(content.len())]).into_owned();
            println!(
                "OVERLAP: {} [{}..{}] {:?}  <->  {} [{}..{}] {:?}",
                p.2, p.0, p.1, text(p.0, p.1),
                entry.2, entry.0, entry.1, text(entry.0, entry.1)
            );
        }
        prev = Some(entry);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: mago-spike <corpus-dir>... | --inspect <file>");
        std::process::exit(2);
    }
    if args[0] == "--inspect" {
        inspect(&args[1]);
        return;
    }

    let mut files: Vec<PathBuf> = vec![];
    for root in &args {
        for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
            if entry.file_type().is_file()
                && entry.path().extension().is_some_and(|e| e == "php")
            {
                files.push(entry.path().to_path_buf());
            }
        }
    }
    files.sort();
    println!("corpus: {} PHP files", files.len());

    // ---- Test A + B over the real corpus ----
    let mut parsed_clean = 0usize;
    let mut with_errors: Vec<(PathBuf, usize)> = vec![];
    let mut tiled_perfect = 0usize;
    let mut gap_files: Vec<(PathBuf, usize, usize, Vec<(usize, usize)>)> = vec![];
    let mut overlap_files: Vec<(PathBuf, usize)> = vec![];
    let mut total_bytes = 0usize;

    let t0 = std::time::Instant::now();
    for path in &files {
        let Ok(content) = std::fs::read(path) else { continue };
        total_bytes += content.len();
        let report = parse_and_analyze(&content, &path.to_string_lossy());
        if report.parse_errors == 0 {
            parsed_clean += 1;
        } else {
            with_errors.push((path.clone(), report.parse_errors));
        }
        if report.gaps.is_empty() && report.overlaps == 0 {
            tiled_perfect += 1;
        } else {
            if !report.gaps.is_empty() {
                let gap_bytes: usize = report.gaps.iter().map(|(s, e)| e - s).sum();
                let mut sample = report.gaps.clone();
                sample.truncate(3);
                gap_files.push((path.clone(), report.gaps.len(), gap_bytes, sample));
            }
            if report.overlaps > 0 {
                overlap_files.push((path.clone(), report.overlaps));
            }
        }
    }
    let elapsed = t0.elapsed();

    println!("\n== Test B: parse over real corpus ==");
    println!(
        "clean: {}/{} ({:.2}%), throughput {:.1} MB/s",
        parsed_clean,
        files.len(),
        100.0 * parsed_clean as f64 / files.len() as f64,
        total_bytes as f64 / 1e6 / elapsed.as_secs_f64()
    );
    for (path, n) in with_errors.iter().take(15) {
        println!("  errors({n}): {}", path.display());
    }
    if with_errors.len() > 15 {
        println!("  ... and {} more", with_errors.len() - 15);
    }

    println!("\n== Test A: span tiling (leaf nodes + trivia) ==");
    println!(
        "perfect tiling: {}/{} ({:.2}%)",
        tiled_perfect,
        files.len(),
        100.0 * tiled_perfect as f64 / files.len() as f64
    );
    println!("files with gaps: {}", gap_files.len());
    for (path, ngaps, gap_bytes, sample) in gap_files.iter().take(10) {
        println!("  {} gaps ({} bytes) in {}: {:?}", ngaps, gap_bytes, path.display(), sample);
    }
    if gap_files.len() > 10 {
        println!("  ... and {} more", gap_files.len() - 10);
    }
    println!("files with overlaps: {}", overlap_files.len());
    for (path, n) in overlap_files.iter().take(5) {
        println!("  overlaps({n}): {}", path.display());
    }

    // ---- Test C: error tolerance on mutated inputs ----
    println!("\n== Test C: error tolerance (mutations of first 300 clean files) ==");
    let clean_files: Vec<&PathBuf> = files
        .iter()
        .filter(|p| !with_errors.iter().any(|(ep, _)| &ep == p))
        .take(300)
        .collect();

    struct MutStats {
        name: &'static str,
        parsed_with_errors: usize,
        parsed_silently: usize,
        node_survival_sum: f64,
        tiled: usize,
        total: usize,
    }
    let mut stats = vec![
        MutStats { name: "truncate@80%", parsed_with_errors: 0, parsed_silently: 0, node_survival_sum: 0.0, tiled: 0, total: 0 },
        MutStats { name: "delete-last-}", parsed_with_errors: 0, parsed_silently: 0, node_survival_sum: 0.0, tiled: 0, total: 0 },
        MutStats { name: "garbage@50%", parsed_with_errors: 0, parsed_silently: 0, node_survival_sum: 0.0, tiled: 0, total: 0 },
        MutStats { name: "del-mid-;", parsed_with_errors: 0, parsed_silently: 0, node_survival_sum: 0.0, tiled: 0, total: 0 },
    ];

    for path in &clean_files {
        let Ok(content) = std::fs::read(path) else { continue };
        if content.len() < 100 {
            continue;
        }
        let name = path.to_string_lossy();
        let baseline = parse_and_analyze(&content, &name);

        let mutations: Vec<Option<Vec<u8>>> = vec![
            Some(content[..content.len() * 8 / 10].to_vec()),
            content
                .iter()
                .rposition(|&b| b == b'}')
                .map(|pos| {
                    let mut m = content.clone();
                    m.remove(pos);
                    m
                }),
            Some({
                // Real garbage: `%^&*` cannot start an expression/statement.
                // (v1 used "@#..." which is legal PHP: error-suppress + hash comment.)
                let mid = content.len() / 2;
                let mut m = content[..mid].to_vec();
                m.extend_from_slice(b" %^&* !!broken!! ");
                m.extend_from_slice(&content[mid..]);
                m
            }),
            // LSP-realistic: delete the first `;` past the 30% mark
            // (mid-editing damage inside one statement).
            content
                .iter()
                .enumerate()
                .skip(content.len() * 3 / 10)
                .find(|&(_, &b)| b == b';')
                .map(|(pos, _)| {
                    let mut m = content.clone();
                    m.remove(pos);
                    m
                }),
        ];

        for (i, mutated) in mutations.into_iter().enumerate() {
            let Some(mutated) = mutated else { continue };
            let report = parse_and_analyze(&mutated, &name);
            stats[i].total += 1;
            if report.parse_errors > 0 {
                stats[i].parsed_with_errors += 1;
            } else {
                stats[i].parsed_silently += 1;
            }
            stats[i].node_survival_sum +=
                report.node_count as f64 / baseline.node_count.max(1) as f64;
            if report.gaps.is_empty() {
                stats[i].tiled += 1;
            }
        }
    }

    for s in &stats {
        println!(
            "  {:14} n={:3}  errors-reported={:.1}%  SILENT-accept={:.1}%  avg-node-survival={:.1}%  still-tiled={:.1}%",
            s.name,
            s.total,
            100.0 * s.parsed_with_errors as f64 / s.total.max(1) as f64,
            100.0 * s.parsed_silently as f64 / s.total.max(1) as f64,
            100.0 * s.node_survival_sum / s.total.max(1) as f64,
            100.0 * s.tiled as f64 / s.total.max(1) as f64,
        );
    }
}

// Appended inspection helper: run with MAGO_SPIKE_INSPECT=<file> to dump
// overlapping leaf spans for one file.
