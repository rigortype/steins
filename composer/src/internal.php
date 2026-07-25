<?php

// Everything the Composer shim does, beyond deciding to do it.
//
// This file is NOT autoloaded. `composer/bin/steins` requires it directly, so a
// project that installs Steins pays nothing for it on any request that is not
// an invocation of the analyzer.
//
// PHP 8.1-compatible syntax throughout, matching the sidecar runner: the shim
// runs on the user's PHP, whatever that is, and the floor is the same one the
// rest of the project states.

declare(strict_types=1);

namespace Steins\Internal;

use Closure;
use Composer\InstalledVersions;
use PharData;
use RuntimeException;

use function array_map;
use function chmod;
use function curl_error;
use function curl_exec;
use function curl_getinfo;
use function curl_init;
use function curl_setopt;
use function escapeshellarg;
use function extension_loaded;
use function fclose;
use function file_exists;
use function file_get_contents;
use function file_put_contents;
use function flock;
use function fopen;
use function fprintf;
use function fwrite;
use function getenv;
use function implode;
use function ini_get;
use function is_dir;
use function is_resource;
use function is_string;
use function ltrim;
use function mkdir;
use function number_format;
use function php_uname;
use function proc_close;
use function proc_get_status;
use function proc_open;
use function str_starts_with;
use function stream_context_create;
use function strtolower;
use function unlink;
use function usleep;

use const CURLINFO_HTTP_CODE;
use const CURLOPT_FILE;
use const CURLOPT_FOLLOWLOCATION;
use const CURLOPT_HTTPHEADER;
use const CURLOPT_NOPROGRESS;
use const CURLOPT_PROGRESSFUNCTION;
use const CURLOPT_USERAGENT;
use const LOCK_EX;
use const LOCK_UN;
use const STDERR;

/**
 * The GitHub repository whose releases hold the binaries.
 *
 * @internal
 */
const REPOSITORY = 'rigortype/steins';

/**
 * Poll interval, in microseconds, while waiting for the analyzer to exit.
 *
 * 10ms keeps the spawn wrapper's own overhead under a tick of a run that takes
 * seconds, without spinning a core for the duration.
 *
 * @internal
 */
const STATUS_CHECK_INTERVAL = 10000;

/**
 * Run a closure while holding an exclusive lock on a file.
 *
 * Blocks until the lock is acquired, and releases it whether the closure
 * returns or throws. Two `vendor/bin/steins` invocations racing on a cold
 * cache — a parallel CI matrix, or a watcher that fires twice — must not both
 * download into the same directory and interleave their extractions.
 *
 * @template T
 *
 * @param string $lockFile Path to the lock file; created when absent.
 * @param Closure(): T $callback Work to perform while the lock is held.
 *
 * @throws RuntimeException If the lock file cannot be opened.
 *
 * @return T
 *
 * @internal
 */
function locked(string $lockFile, Closure $callback)
{
    $handle = fopen($lockFile, 'c');
    if ($handle === false) {
        throw new RuntimeException("unable to create lock file: {$lockFile}");
    }

    flock($handle, LOCK_EX);

    try {
        return $callback();
    } finally {
        flock($handle, LOCK_UN);
        fclose($handle);
    }
}

/**
 * The installed package version, normalized to bare `X.Y.Z`.
 *
 * Composer reports the version as it appears in the tag, and Steins tags
 * releases as `vX.Y.Z`. Strip the prefix here, once, and let every caller that
 * needs the tag form add it back via {@see tag_for()} — the two forms both
 * appear in a download URL (the tag names the release, the archive embeds it),
 * so leaving them tangled is how one of them ends up wrong.
 *
 * @throws RuntimeException If the version is unknown, or is not a released one.
 *
 * @internal
 */
function get_version(): string
{
    $pretty = InstalledVersions::getPrettyVersion(REPOSITORY);
    if ($pretty === null) {
        throw new RuntimeException('could not determine the installed steins version from Composer metadata.');
    }

    // A branch install (`dev-master`, `dev-feature/x`) has no corresponding
    // GitHub Release, so there is no binary to fetch. Say so, rather than
    // constructing a URL that is certain to 404.
    if (str_starts_with($pretty, 'dev-')) {
        throw new RuntimeException(
            "installed from a branch ({$pretty}), which has no released binary. Require a tagged version, or "
            . 'build from source: cargo install --git https://github.com/' . REPOSITORY . ' steins-cli',
        );
    }

    return ltrim($pretty, 'v');
}

/**
 * The release tag for a normalized version.
 *
 * @internal
 */
function tag_for(string $version): string
{
    return "v{$version}";
}

/**
 * The Rust target triple for the machine this is running on.
 *
 * Only targets with a published release binary are returned. Anything else
 * raises, naming what was detected and the source-build fallback — a wrong
 * guess here does not fail at detection, it fails at exec with a dynamic-loader
 * error that says nothing about the actual cause.
 *
 * @throws RuntimeException If no released binary matches this machine.
 *
 * @internal
 */
function detect_target(): string
{
    $machine = strtolower(php_uname('m'));
    $system = strtolower(php_uname('s'));

    $architecture = match ($machine) {
        'arm64', 'aarch64' => 'aarch64',
        'x86_64', 'amd64' => 'x86_64',
        default => null,
    };

    if ($architecture === 'aarch64' && $system === 'darwin') {
        return 'aarch64-apple-darwin';
    }

    throw new RuntimeException(
        "no released binary for this platform ({$system} {$machine}). Install via Homebrew, or build from source: "
        . 'cargo install --git https://github.com/' . REPOSITORY . ' steins-cli',
    );
}

/**
 * The release asset name for a version and target.
 *
 * Mirrors the `archive: $bin-$tag-$target` input the release workflow passes to
 * upload-rust-binary-action. The tag form (`v0.1.0`), not the bare version, is
 * what appears in the asset name.
 *
 * @internal
 */
function archive_name(string $version, string $target): string
{
    return 'steins-' . namespace\tag_for($version) . "-{$target}.tar.gz";
}

/**
 * The download URL for a release asset.
 *
 * @internal
 */
function build_download_url(string $version, string $asset): string
{
    return 'https://github.com/' . REPOSITORY . '/releases/download/' . namespace\tag_for($version) . "/{$asset}";
}

/**
 * A GitHub token from the environment, when one is set.
 *
 * Anonymous requests to github.com are rate-limited per IP, which a busy shared
 * CI runner can exhaust. Sending a token that the environment already holds
 * costs nothing and avoids a failure that looks like an outage.
 *
 * @return null|string The token, or null when neither variable is set.
 *
 * @internal
 */
function get_github_token(): ?string
{
    foreach (['GITHUB_TOKEN', 'GH_TOKEN'] as $variable) {
        $value = getenv($variable);
        if (is_string($value) && $value !== '') {
            return $value;
        }
    }

    return null;
}

/**
 * Download a URL to a path, by whatever means this PHP provides.
 *
 * @throws RuntimeException If the download fails, or neither method is available.
 *
 * @internal
 */
function download(string $url, string $destination): void
{
    if (extension_loaded('curl')) {
        namespace\download_with_curl($url, $destination);

        return;
    }

    if (ini_get('allow_url_fopen')) {
        namespace\download_with_fopen($url, $destination);

        return;
    }

    throw new RuntimeException(
        'cannot download the analyzer: install the PHP curl extension, or set allow_url_fopen=1 in php.ini.',
    );
}

/**
 * Download via ext-curl, reporting progress.
 *
 * @throws RuntimeException If the transfer fails or the server returns an error status.
 *
 * @internal
 */
function download_with_curl(string $url, string $destination): void
{
    $handle = curl_init($url);
    if ($handle === false) {
        throw new RuntimeException('cannot initialize curl.');
    }

    $file = fopen($destination, 'w');
    if ($file === false) {
        throw new RuntimeException("cannot write to {$destination}");
    }

    curl_setopt($handle, CURLOPT_FOLLOWLOCATION, true);
    curl_setopt($handle, CURLOPT_FILE, $file);
    curl_setopt($handle, CURLOPT_NOPROGRESS, false);
    curl_setopt($handle, CURLOPT_USERAGENT, 'steins-composer');

    $token = namespace\get_github_token();
    if ($token !== null) {
        curl_setopt($handle, CURLOPT_HTTPHEADER, ['Authorization: Bearer ' . $token]);
    }

    // Progress goes to stderr so it can never contaminate a `--format json` run
    // that a caller is piping.
    curl_setopt($handle, CURLOPT_PROGRESSFUNCTION, static function ($_handle, int $total, int $sofar): int {
        if ($total > 0) {
            fprintf(
                STDERR,
                "\r  %s / %s MB (%d%%)",
                number_format($sofar / 1048576, 1),
                number_format($total / 1048576, 1),
                (int) (($sofar / $total) * 100),
            );
        }

        return 0;
    });

    $ok = curl_exec($handle);
    $status = (int) curl_getinfo($handle, CURLINFO_HTTP_CODE);
    $error = curl_error($handle);
    fclose($file);
    fprintf(STDERR, "\n");

    if ($ok === false || $status >= 400) {
        unlink($destination);

        throw new RuntimeException(namespace\download_failure_message($url, $status, $error));
    }
}

/**
 * Download via `file_get_contents` (requires `allow_url_fopen`).
 *
 * @throws RuntimeException If the transfer fails.
 *
 * @internal
 */
function download_with_fopen(string $url, string $destination): void
{
    $headers = ['User-Agent: steins-composer'];

    $token = namespace\get_github_token();
    if ($token !== null) {
        $headers[] = 'Authorization: Bearer ' . $token;
    }

    $context = stream_context_create([
        'http' => [
            'method' => 'GET',
            'header' => implode("\r\n", $headers),
            'follow_location' => 1,
            'max_redirects' => 20,
        ],
    ]);

    $contents = file_get_contents($url, false, $context);
    if ($contents === false) {
        throw new RuntimeException(namespace\download_failure_message($url, 0, ''));
    }

    file_put_contents($destination, $contents);
}

/**
 * Phrase a failed download.
 *
 * A 404 here is ambiguous in a way worth spelling out. The release workflow
 * builds each target in a job with `fail-fast: false`, so a tag can exist —
 * and Packagist can already be serving it — while a given target's asset is
 * still uploading, or has failed and is awaiting a re-run. "Not found" alone
 * reads as an unsupported platform, which is a different problem with a
 * different fix.
 *
 * @internal
 */
function download_failure_message(string $url, int $status, string $error): string
{
    $detail = $status > 0 ? "HTTP {$status}" : 'transfer failed';
    if ($error !== '') {
        $detail .= ": {$error}";
    }

    $message = "could not download the analyzer ({$detail})\n  {$url}";

    if ($status === 404) {
        $message .= "\n  The release may have been published moments ago and this target's archive may not be "
            . 'uploaded yet — retry shortly. If it persists, the target failed to build for this release; report it '
            . 'at https://github.com/' . REPOSITORY . '/issues';
    }

    return $message;
}

/**
 * Extract a `.tar.gz` archive into a directory.
 *
 * @throws RuntimeException If the archive cannot be read.
 *
 * @internal
 */
function extract_archive(string $archiveFile, string $destination): void
{
    if (!extension_loaded('phar')) {
        throw new RuntimeException('cannot extract the analyzer archive: the PHP phar extension is not loaded.');
    }

    $phar = new PharData($archiveFile);
    // Overwrite: a previous run may have been interrupted between extracting
    // and verifying, leaving a partial directory that would otherwise make
    // every later run fail on an existing file.
    $phar->extractTo($destination, null, true);

    unlink($archiveFile);
}

/**
 * Path to the analyzer binary for this version and target, fetching it if needed.
 *
 * The cache is keyed by version and target, so `composer update` to a new
 * version fetches the new binary rather than silently reusing the old one, and
 * two targets sharing a checkout (a mounted volume between host and container)
 * do not evict each other.
 *
 * @param string $version Normalized package version.
 * @param string $target Rust target triple.
 * @param string $binDir This package's `composer/bin` directory.
 *
 * @throws RuntimeException If the download or extraction fails.
 *
 * @internal
 */
function ensure_binary(string $version, string $target, string $binDir): string
{
    $versionDir = "{$binDir}/{$version}";
    $targetDir = "{$versionDir}/{$target}";
    // The release archive holds the bare binary at its root, beside the
    // notices — the same layout the Homebrew formula's `bin.install "steins"`
    // relies on. The extraction directory is ours to name, because the archive
    // does not carry one.
    $executable = "{$targetDir}/steins";

    if (file_exists($executable)) {
        return $executable;
    }

    if (!is_dir($versionDir)) {
        mkdir($versionDir, 0755, true);
    }

    // Lock per target, so a host and a container sharing this directory but
    // needing different binaries do not serialize behind each other.
    $lockFile = "{$versionDir}/.steins-{$target}.lock";

    return namespace\locked($lockFile, static function () use (
        $version,
        $target,
        $versionDir,
        $targetDir,
        $executable
    ): string {
        // Re-check under the lock: whoever held it before us may have been
        // downloading exactly this.
        if (file_exists($executable)) {
            return $executable;
        }

        $asset = namespace\archive_name($version, $target);
        $archiveFile = "{$versionDir}/{$asset}";

        fprintf(STDERR, "Downloading steins %s for %s...\n", $version, $target);
        namespace\download(namespace\build_download_url($version, $asset), $archiveFile);
        namespace\extract_archive($archiveFile, $targetDir);

        if (!file_exists($executable)) {
            throw new RuntimeException("the archive did not contain the expected binary at {$executable}");
        }

        chmod($executable, 0755);

        return $executable;
    });
}

/**
 * Run the analyzer and exit with its status. Does not return.
 *
 * The three standard streams are inherited rather than piped, so the analyzer
 * writes straight to the caller's terminal: colour detection, progress
 * rendering, and a piped `--format json` all behave as they do for a
 * Homebrew-installed binary. This shim is a launcher, not a filter.
 *
 * @param list<string> $args Arguments to forward.
 *
 * @return never
 *
 * @internal
 */
function execute(string $executable, array $args): never
{
    $command = escapeshellarg($executable);
    if ($args !== []) {
        $command .= ' ' . implode(' ', array_map(escapeshellarg(...), $args));
    }

    $pipes = [];
    $process = proc_open(
        $command,
        [
            0 => ['file', 'php://stdin', 'r'],
            1 => ['file', 'php://stdout', 'w'],
            2 => ['file', 'php://stderr', 'w'],
        ],
        $pipes,
    );

    if (!is_resource($process)) {
        fwrite(STDERR, "steins: unable to start the analyzer process.\n");
        exit(1);
    }

    do {
        usleep(STATUS_CHECK_INTERVAL);
        $status = proc_get_status($process);
    } while ($status['running']);

    // Steins gives exit codes meaning (ADR-0050 §7): they distinguish "findings
    // were reported" from "the run failed", and CI reads them. Forwarding the
    // code unchanged is the contract this shim must not break. A signalled
    // death becomes 128+signum, the shell convention.
    $code = $status['signaled'] ? $status['termsig'] + 128 : $status['exitcode'];

    proc_close($process);
    exit($code);
}
