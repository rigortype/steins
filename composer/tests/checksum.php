#!/usr/bin/env php
<?php

// Reading a checksum sidecar, and refusing everything that is not one.
//
// The sidecar's format and naming come from a third-party action, so the
// interesting cases are the malformed ones: a checksum that is not a checksum
// verifies nothing while looking like it does, and that is the failure this
// whole slice exists to prevent.
//
// The happy-path digest is the one the v0.1.0 release actually published, so
// the well-formed case is the real format rather than a guess at it:
//
//     gh release download v0.1.0 --repo rigortype/steins \
//       --pattern 'steins-v0.1.0-aarch64-apple-darwin.sha256' -O -
//
//     php composer/tests/checksum.php

declare(strict_types=1);

namespace Steins\Internal;

use RuntimeException;

use function file_put_contents;
use function printf;
use function str_contains;
use function str_repeat;
use function sys_get_temp_dir;
use function uniqid;

require __DIR__ . '/../src/internal.php';

$failures = 0;
$checks = 0;

$sidecarWith = static function (string $contents): string {
    $path = sys_get_temp_dir() . '/steins-sidecar-' . uniqid();
    file_put_contents($path, $contents);

    return $path;
};

$expect = static function (string $label, string $expected, string $actual) use (&$failures, &$checks): void {
    $checks++;
    if ($expected === $actual) {
        printf("  ok    %s\n", $label);

        return;
    }

    $failures++;
    printf("  FAIL  %s -> %s (expected %s)\n", $label, $actual, $expected);
};

$expectRefusal = static function (string $label, string $needle, callable $call) use (&$failures, &$checks): void {
    $checks++;
    try {
        $call();
    } catch (RuntimeException $e) {
        if (str_contains($e->getMessage(), $needle)) {
            printf("  ok    %s\n", $label);

            return;
        }

        $failures++;
        printf("  FAIL  %s -> refused, but the message never mentions '%s': %s\n", $label, $needle, $e->getMessage());

        return;
    }

    $failures++;
    printf("  FAIL  %s -> accepted, expected a refusal\n", $label);
};

$asset = 'steins-v0.1.0-aarch64-apple-darwin.tar.gz';
$real = '2a7aec57a8e67a73c76402865f4d307b4cb52a6b593fad9883dca419b83fced8';
$other = str_repeat('b', 64);

printf("The sidecar naming that is easy to get backwards\n");
$expect(
    'sidecar drops .tar.gz, archive keeps it',
    'steins-v0.1.0-aarch64-apple-darwin.sha256',
    sidecar_name('0.1.0', 'aarch64-apple-darwin'),
);
$expect('archive keeps it', $asset, archive_name('0.1.0', 'aarch64-apple-darwin'));

printf("\nReading a well-formed sidecar\n");
// Exactly what the release published: digest, two spaces, archive name.
$expect('the real v0.1.0 sidecar', $real, expected_digest($sidecarWith("{$real}  {$asset}\n"), $asset));
$expect('binary-mode * prefix', $real, expected_digest($sidecarWith("{$real} *{$asset}\n"), $asset));
$expect('single space', $real, expected_digest($sidecarWith("{$real} {$asset}\n"), $asset));
$expect('no trailing newline', $real, expected_digest($sidecarWith("{$real}  {$asset}"), $asset));
// The reason the line is selected by name rather than by position: one sidecar
// can cover several assets if another archive format is ever enabled upstream.
$expect(
    'multi-line, ours is second',
    $real,
    expected_digest($sidecarWith("{$other}  steins-v0.1.0-aarch64-apple-darwin.tar.xz\n{$real}  {$asset}\n"), $asset),
);

printf("\nRefusing a sidecar that would verify nothing\n");
$expectRefusal(
    'no entry for our archive',
    'no entry',
    static fn(): string => expected_digest($sidecarWith("{$other}  some-other-file.tar.gz\n"), $asset),
);
$expectRefusal(
    'empty file',
    'no entry',
    static fn(): string => expected_digest($sidecarWith(''), $asset),
);
$expectRefusal(
    'digest too short',
    'not a sha256',
    static fn(): string => expected_digest($sidecarWith("abc123  {$asset}\n"), $asset),
);
$expectRefusal(
    'digest not hex',
    'not a sha256',
    static fn(): string => expected_digest($sidecarWith(str_repeat('z', 64) . "  {$asset}\n"), $asset),
);
$expectRefusal(
    'uppercase digest is not the published form',
    'not a sha256',
    static fn(): string => expected_digest($sidecarWith(str_repeat('A', 64) . "  {$asset}\n"), $asset),
);

printf("\nVerifying an archive against a digest\n");
$good = $sidecarWith('the payload');
$goodDigest = hash_file('sha256', $good);
$checks++;
try {
    verify_digest($good, $goodDigest, $asset);
    printf("  ok    a matching digest passes\n");
} catch (RuntimeException $e) {
    $failures++;
    printf("  FAIL  a matching digest was rejected: %s\n", $e->getMessage());
}

$bad = $sidecarWith('the payload, tampered with');
$expectRefusal('a mismatched digest fails', 'does not match', static function () use ($bad, $goodDigest, $asset): void {
    verify_digest($bad, $goodDigest, $asset);
});
// The rejected download must not survive: a retry has to start from clean bytes
// rather than re-verifying the same bad ones.
$checks++;
if (!file_exists($bad)) {
    printf("  ok    the rejected archive is deleted\n");
} else {
    $failures++;
    printf("  FAIL  the rejected archive was left on disk\n");
}

printf("\n%d checks, %d failed\n", $checks, $failures);

exit($failures === 0 ? 0 : 1);
