//! The ADR-0101 witness table against the engine: a literal printf format reads
//! the locale exactly when its output moves between `C` and `de_DE.UTF-8`, for
//! `sprintf` and `vsprintf` alike (issue #991). Every test skips, loudly, without
//! `php` on the PATH or without the `de_DE.UTF-8` locale installed, unless `CI` is
//! set: the CI test job installs both, so there a missing one fails the test
//! rather than letting a green run mean nothing.

use std::io::Write as _;
use std::process::{Command, Stdio};

use steins_catalog::format_reads_locale;

/// Whether the oracle cannot run: a loud skip off CI, a failure on CI.
fn oracle_unavailable(reason: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "the locale oracle cannot run on CI: {reason}; the test job installs php and generates de_DE.UTF-8"
    );
    eprintln!("SKIP: {reason}; oracle comparison not run");
}

/// For each format, whether `sprintf` of floats moves between `C` and
/// `de_DE.UTF-8`, and whether `vsprintf` gave `sprintf`'s answer in both
/// locales. Two values are rendered, `1234.5` and `0.123456`, and a format
/// counts as moved when either moves, since `%.4g` of the first prints no
/// decimal point. `None` (a skip) without `php` or the locale.
fn engine_locale_moves(formats: &[String]) -> Option<Vec<(bool, bool)>> {
    if Command::new("php").arg("--version").output().is_err() {
        oracle_unavailable("php is not on PATH");
        return None;
    }
    // One format per line: none of them holds a newline.
    let script = r#"
        $formats = explode("\n", rtrim(stream_get_contents(STDIN), "\n"));
        if (setlocale(LC_ALL, 'de_DE.UTF-8') === false) { echo "NOLOCALE\n"; exit; }
        $run = function (string $locale, string $format): array {
            setlocale(LC_ALL, $locale);
            $s = $v = '';
            foreach ([1234.5, 0.123456] as $x) {
                $values = array_fill(0, 8, $x);
                try { $s .= @sprintf($format, ...$values) . '|'; } catch (\Throwable $e) { $s .= 'ERR'; }
                try { $v .= @vsprintf($format, $values) . '|'; } catch (\Throwable $e) { $v .= 'ERR'; }
            }
            return [$s, $v];
        };
        foreach ($formats as $format) {
            [$cs, $cv] = $run('C', $format);
            [$ds, $dv] = $run('de_DE.UTF-8', $format);
            echo ($cs !== $ds ? '1' : '0'), ($cs === $cv && $ds === $dv ? '1' : '0'), "\n";
        }
    "#;
    let mut child = Command::new("php")
        .args(["-d", "display_errors=stderr", "-r", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn php");
    let payload = formats.join("\n");
    child.stdin.take().expect("stdin").write_all(payload.as_bytes()).expect("write");
    let out = child.wait_with_output().expect("php run");
    assert!(out.status.success(), "php failed");
    let text = String::from_utf8(out.stdout).expect("utf8");
    if text.starts_with("NOLOCALE") {
        oracle_unavailable("de_DE.UTF-8 is not installed");
        return None;
    }
    Some(text.lines().map(|l| (l.starts_with('1'), l.ends_with('1'))).collect())
}

/// C against `de_DE` on every conversion letter php-src's formatter knows, under
/// a spread of flags, padding, widths, precisions and positions, plus whole
/// formats mixing conversions with text and `%%`.
///
/// Soundness (a moved output must have been called a read) is asserted on the
/// whole grid. Precision (a read must have moved) is asserted wherever the
/// output shows a decimal point to move, which a precision of zero never does.
#[test]
fn the_locale_verdict_matches_the_engine_under_de_de() {
    let mut formats: Vec<String> = Vec::new();
    for letter in "bcdeEfFgGosuxXhH".chars() {
        for m in [
            "", "5", "-8", "+", "05", ".0", ".1", ".2", ".10", "1$", "1$.3", "'*8", "+010.4", " ",
        ] {
            formats.push(format!("%{m}{letter}"));
        }
    }
    for format in ["%%f", "%d %f", "%d-%s", "%s%%f", "%1$s %1$.2f", "%%%g", "%5.1f%%", "a%G"] {
        formats.push(format.to_owned());
    }
    let Some(moves) = engine_locale_moves(&formats) else { return };
    assert_eq!(moves.len(), formats.len());
    for (format, (moved, vector_agrees)) in formats.iter().zip(moves) {
        let reads = format_reads_locale(format);
        assert!(vector_agrees, "{format}: vsprintf did not follow sprintf");
        assert!(reads || !moved, "{format} moved under de_DE and was called locale-free");
        // A precision of zero prints no decimal point whatever the value.
        let no_point = matches!(format.as_str(), "%.0f" | "%.0g" | "%.0G");
        if reads && !no_point {
            assert!(moved, "{format} was called a locale read and did not move");
        }
    }
}

/// The two facts the ADR's lexical rule rests on, stated against the engine on
/// their own: `%%f` is no conversion, so it never moves, and `%F` is `%f`
/// without the locale, so it never moves while `%f` does.
#[test]
fn an_escaped_percent_and_the_capital_f_do_not_move() {
    let formats: Vec<String> = ["%%f", "%F", "%.3F", "%f", "%.3f"].map(str::to_owned).to_vec();
    let Some(moves) = engine_locale_moves(&formats) else { return };
    let moved: Vec<bool> = moves.iter().map(|m| m.0).collect();
    assert_eq!(moved, [false, false, false, true, true]);
    let reads: Vec<bool> = formats.iter().map(|f| format_reads_locale(f)).collect();
    assert_eq!(reads, [false, false, false, true, true]);
}
