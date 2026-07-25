#!/usr/bin/env php
<?php

// Target detection over the full matrix, including machines this host is not.
//
// Detection is the one part of the shim that cannot be exercised by running it:
// a macOS developer never takes the musl branch, and CI on an x86_64 runner
// never takes the arm64 one. Every wrong answer here costs a user a download
// that succeeds and then fails at exec, so the branches are checked against
// fixture roots that stand in for the filesystems that distinguish them.
//
// What this does NOT establish: that the chosen archive actually runs on the
// machine that chose it. Only a real runner shows that, which is the CI smoke
// slice.
//
//     php composer/tests/target-detection.php

declare(strict_types=1);

namespace Steins\Internal;

use function count;
use function is_dir;
use function mkdir;
use function printf;
use function sprintf;
use function sys_get_temp_dir;
use function touch;
use function uniqid;

require __DIR__ . '/../src/internal.php';

$failures = 0;
$checks = 0;

/**
 * A fixture root containing the loader files that identify a libc.
 *
 * @param list<string> $files Absolute-looking paths to create under the root.
 */
$rootWith = static function (array $files): string {
    $root = sys_get_temp_dir() . '/steins-detect-' . uniqid();
    foreach ($files as $file) {
        $path = $root . $file;
        $directory = dirname($path);
        if (!is_dir($directory)) {
            mkdir($directory, 0755, true);
        }

        touch($path);
    }

    // A root with no marker files at all still has to exist.
    if (!is_dir($root)) {
        mkdir($root, 0755, true);
    }

    return $root;
};

$expect = static function (string $label, string $expected, string $actual) use (&$failures, &$checks): void {
    $checks++;
    if ($expected === $actual) {
        printf("  ok    %s -> %s\n", $label, $actual);

        return;
    }

    $failures++;
    printf("  FAIL  %s -> %s (expected %s)\n", $label, $actual, $expected);
};

$expectRefusal = static function (string $label, string $needle, callable $call) use (&$failures, &$checks): void {
    $checks++;
    try {
        $result = $call();
    } catch (\RuntimeException $e) {
        if (str_contains($e->getMessage(), $needle)) {
            printf("  ok    %s -> refused\n", $label);

            return;
        }

        $failures++;
        printf("  FAIL  %s -> refused, but the message never mentions %s\n", $label, $needle);

        return;
    }

    $failures++;
    printf("  FAIL  %s -> returned %s, expected a refusal\n", $label, $result);
};

$glibc = $rootWith(['/lib64/ld-linux-x86-64.so.2']);
$glibcArm = $rootWith(['/lib/ld-linux-aarch64.so.1']);
$alpine = $rootWith(['/etc/alpine-release', '/lib/ld-musl-x86_64.so.1']);
$alpineArm = $rootWith(['/etc/alpine-release', '/lib/ld-musl-aarch64.so.1']);
$muslNoRelease = $rootWith(['/lib/ld-musl-x86_64.so.1']);
$bare = $rootWith([]);

printf("macOS — no libc question to answer\n");
$expect('darwin arm64', 'aarch64-apple-darwin', detect_target('arm64', 'Darwin'));
$expect('darwin x86_64', 'x86_64-apple-darwin', detect_target('x86_64', 'Darwin'));

printf("\nLinux x86_64 — both builds exist, so detection picks\n");
$expect('glibc', 'x86_64-unknown-linux-gnu', detect_target('x86_64', 'Linux', $glibc));
$expect('alpine', 'x86_64-unknown-linux-musl', detect_target('x86_64', 'Linux', $alpine));
$expect('musl, no /etc/alpine-release', 'x86_64-unknown-linux-musl', detect_target('x86_64', 'Linux', $muslNoRelease));
// The tie-break that matters: the musl build is static and runs on a glibc
// host, so an inconclusive probe must not gamble on the glibc one.
$expect('nothing detectable', 'x86_64-unknown-linux-musl', detect_target('x86_64', 'Linux', $bare));
$expect('amd64 spelling', 'x86_64-unknown-linux-gnu', detect_target('amd64', 'Linux', $glibc));

printf("\nLinux arm64 — glibc only, and the gap is refused by name\n");
$expect('glibc', 'aarch64-unknown-linux-gnu', detect_target('aarch64', 'Linux', $glibcArm));
$expect('nothing detectable', 'aarch64-unknown-linux-gnu', detect_target('aarch64', 'Linux', $bare));
$expectRefusal('alpine arm64', 'musl', static fn(): string => detect_target('aarch64', 'Linux', $alpineArm));

printf("\nPlatforms with no row at all\n");
$expectRefusal('windows', 'Windows', static fn(): string => detect_target('x86_64', 'Windows NT'));
$expectRefusal('freebsd', 'freebsd', static fn(): string => detect_target('x86_64', 'FreeBSD'));
$expectRefusal('32-bit arm', 'armv7l', static fn(): string => detect_target('armv7l', 'Linux'));
$expectRefusal('i686', 'i686', static fn(): string => detect_target('i686', 'Linux'));

printf("\nAsset names match what the release workflow uploads\n");
foreach (
    [
        'x86_64-apple-darwin',
        'aarch64-apple-darwin',
        'x86_64-unknown-linux-gnu',
        'aarch64-unknown-linux-gnu',
        'x86_64-unknown-linux-musl',
    ] as $target
) {
    $expect($target, "steins-v9.9.9-{$target}.tar.gz", archive_name('9.9.9', $target));
}

printf("\n%d checks, %d failed\n", $checks, $failures);

exit($failures === 0 ? 0 : 1);
