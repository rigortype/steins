#!/usr/bin/env php
<?php

// What the shim does after it has a binary: hand the process over and get out
// of the way.
//
// The download half of this file has visible failure modes — a wrong URL 404s,
// a bad checksum refuses. The launch half does not. A stream the analyzer
// cannot detect as a terminal silently loses colour; a mangled exit code
// silently turns a CI failure green; an unforwarded signal silently leaves the
// analyzer running after the wrapper has gone, writing into a terminal that has
// already returned a prompt. None of it announces itself, so it is asserted
// here instead.
//
// Both launch paths are exercised, because a user gets whichever their PHP
// gives them: `pcntl_exec()` where ext-pcntl is available, and the `proc_open()`
// fallback where it is not. The second is forced with `disable_functions`, so
// the row runs on every host rather than only on one that happens to lack pcntl.
//
//     php composer/tests/launcher.php

declare(strict_types=1);

namespace Steins\Internal;

use function array_map;
use function count;
use function explode;
use function file_exists;
use function file_get_contents;
use function file_put_contents;
use function function_exists;
use function implode;
use function is_dir;
use function is_resource;
use function mkdir;
use function printf;
use function proc_close;
use function proc_get_status;
use function proc_open;
use function proc_terminate;
use function rtrim;
use function sys_get_temp_dir;
use function uniqid;
use function usleep;

use const PHP_BINARY;

$failures = 0;
$checks = 0;

$expect = static function (string $label, string $expected, string $actual) use (&$failures, &$checks): void {
    $checks++;
    if ($expected === $actual) {
        printf("  ok    %s\n", $label);

        return;
    }

    $failures++;
    printf("  FAIL  %s\n          expected  %s\n          actual    %s\n", $label, $expected, $actual);
};

// ─────────────────────────────────────────────────────────────────────────────
// The fixture: a stand-in analyzer, and the shim calling execute() on it.
//
// The stand-in is PHP rather than a shell script so that it behaves the same on
// Alpine as on macOS, and so that the arguments it reports are a real argv
// rather than something a shell has already had an opinion about.

$workspace = sys_get_temp_dir() . '/steins-launcher-' . uniqid();
if (!is_dir($workspace)) {
    mkdir($workspace, 0755, true);
}

$analyzer = "{$workspace}/analyzer.php";
file_put_contents($analyzer, <<<'PHP'
    <?php
    $args = $argv;
    array_shift($args);
    $mode = array_shift($args) ?? '';

    if ($mode === 'streams') {
        fwrite(STDOUT, "to-stdout\n");
        fwrite(STDERR, "to-stderr\n");
        exit(0);
    }

    if ($mode === 'args') {
        foreach ($args as $arg) {
            fwrite(STDOUT, "[{$arg}]\n");
        }
        exit(0);
    }

    if ($mode === 'exit') {
        exit((int) $args[0]);
    }

    if ($mode === 'signal') {
        posix_kill(getmypid(), SIGTERM);
        sleep(10);
        exit(0);
    }

    if ($mode === 'longrun') {
        // A run that keeps writing to the caller's stdout, so that "did it stop
        // when the wrapper did" is a question the file can answer.
        for ($i = 0; $i < 100; $i++) {
            fwrite(STDOUT, "tick\n");
            usleep(100000);
        }
        exit(0);
    }

    fwrite(STDERR, "unknown mode: {$mode}\n");
    exit(64);
    PHP);

$runner = "{$workspace}/runner.php";
$internal = __DIR__ . '/../src/internal.php';
file_put_contents($runner, <<<PHP
    <?php
    require '{$internal}';
    \$args = \$argv;
    array_shift(\$args);
    \$executable = array_shift(\$args);
    Steins\\Internal\\execute(\$executable, \$args);
    PHP);

/**
 * Run the shim's execute() in a subprocess, capturing the two streams apart.
 *
 * `$ini` is how the fallback row is reached on a host that has ext-pcntl: the
 * launch path is chosen by function_exists(), and disable_functions is what
 * makes that answer no.
 *
 * @param list<string> $args Arguments for the stand-in analyzer.
 * @param list<string> $ini `-d` arguments for the wrapper's own PHP.
 *
 * @return array{code: int, out: string, err: string}
 */
$run = static function (array $args, array $ini = []) use ($runner, $analyzer, $workspace): array {
    $out = "{$workspace}/out-" . uniqid();
    $err = "{$workspace}/err-" . uniqid();

    $command = [PHP_BINARY, ...$ini, $runner, PHP_BINARY, $analyzer, ...$args];
    $pipes = [];
    $process = proc_open(
        $command,
        [0 => ['file', '/dev/null', 'r'], 1 => ['file', $out, 'w'], 2 => ['file', $err, 'w']],
        $pipes,
    );

    if (!is_resource($process)) {
        return ['code' => -1, 'out' => '', 'err' => 'could not start the wrapper'];
    }

    do {
        usleep(10000);
        $status = proc_get_status($process);
    } while ($status['running']);

    proc_close($process);

    return [
        'code' => $status['signaled'] ? $status['termsig'] + 128 : $status['exitcode'],
        'out' => file_exists($out) ? (string) file_get_contents($out) : '',
        'err' => file_exists($err) ? (string) file_get_contents($err) : '',
    ];
};

/**
 * Start execute() in the background, kill the WRAPPER, and report what the
 * analyzer did next.
 *
 * The measurement that matters is the second line count. A wrapper that dies
 * without taking the analyzer with it leaves a process that still holds the
 * caller's stdout, and the output keeps arriving after the shell prompt is back
 * or after CI has recorded the step as finished.
 *
 * @param list<string> $ini `-d` arguments for the wrapper's own PHP.
 *
 * @return array{code: int, at_death: int, after: int}
 */
$orphanCheck = static function (array $ini = []) use ($runner, $analyzer, $workspace): array {
    $out = "{$workspace}/orphan-" . uniqid();

    $pipes = [];
    $process = proc_open(
        [PHP_BINARY, ...$ini, $runner, PHP_BINARY, $analyzer, 'longrun'],
        [0 => ['file', '/dev/null', 'r'], 1 => ['file', $out, 'w'], 2 => ['file', $out, 'a']],
        $pipes,
    );

    $lines = static fn(): int => file_exists($out)
        ? count(explode("\n", rtrim((string) file_get_contents($out), "\n")))
        : 0;

    // Long enough for the analyzer to be past its first few writes, so that a
    // stalled process and a stopped one do not look alike.
    usleep(600000);

    // SIGTERM to the wrapper's pid alone. This is the shape `timeout`,
    // `docker stop` and a cancelled CI job all have; Ctrl-C is not, because a
    // terminal signals the whole foreground process group.
    proc_terminate($process, 15);

    do {
        usleep(10000);
        $status = proc_get_status($process);
    } while ($status['running']);

    $code = $status['signaled'] ? $status['termsig'] + 128 : $status['exitcode'];
    proc_close($process);

    $atDeath = $lines();
    usleep(1200000);

    return ['code' => $code, 'at_death' => $atDeath, 'after' => $lines()];
};

// ─────────────────────────────────────────────────────────────────────────────

$rows = ['pcntl_exec' => []];
if (function_exists('pcntl_exec')) {
    $rows['proc_open fallback'] = ['-d', 'disable_functions=pcntl_exec'];
} else {
    printf("ext-pcntl is not available here, so there is only one path to test.\n\n");
}

foreach ($rows as $path => $ini) {
    printf("%s\n", $path);

    $streams = $run(['streams'], $ini);
    $expect("{$path}: stdout carries only the analyzer's stdout", "to-stdout\n", $streams['out']);
    $expect("{$path}: stderr carries only the analyzer's stderr", "to-stderr\n", $streams['err']);

    // Exit codes are a contract (ADR-0050 §7): they distinguish "findings were
    // reported" from "the run failed", and CI reads them. 2 is the findings
    // code, and the one a wrapper that clamps to 0/1 would destroy.
    foreach ([0, 1, 2, 42] as $code) {
        $result = $run(['exit', (string) $code], $ini);
        $expect("{$path}: exit {$code} survives", (string) $code, (string) $result['code']);
    }

    // A launcher that runs the analyzer through a shell would expand these, or
    // quote them wrong. Nothing here is allowed to interpret them.
    $awkward = ['a b', "it's", '$HOME', '*', 'trailing\\'];
    $args = $run(['args', ...$awkward], $ini);
    $expected = implode('', array_map(static fn(string $a): string => "[{$a}]\n", $awkward));
    $expect("{$path}: arguments reach the analyzer verbatim", $expected, $args['out']);

    if (function_exists('posix_kill')) {
        // A signalled analyzer reports as 128+signum, the shell convention, and
        // NOT as the exit code of a wrapper that outlived it.
        $signalled = $run(['signal'], $ini);
        $expect("{$path}: a signalled analyzer reports 143", '143', (string) $signalled['code']);
    } else {
        printf("  skip  %s: signalled-analyzer row needs ext-posix\n", $path);
    }

    $orphan = $orphanCheck($ini);
    $expect(
        "{$path}: SIGTERM to the wrapper stops the analyzer too",
        'stopped',
        $orphan['after'] === $orphan['at_death']
            ? 'stopped'
            : "still writing ({$orphan['at_death']} lines at the wrapper's death, {$orphan['after']} after)",
    );
    $expect("{$path}: and the wrapper reports 143 for it", '143', (string) $orphan['code']);

    printf("\n");
}

printf("%d checks, %d failed\n", $checks, $failures);

exit($failures === 0 ? 0 : 1);
