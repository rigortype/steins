<?php

/**
 * Mine every ENGINE-DEFINED constant off the resident PHP, for ADR-0094 §2.
 *
 * Reads `get_defined_constants(true)`, which groups by the extension that
 * registered the constant — the grouping is what lets the caller keep the
 * catalog's extension set and nothing else, and what makes "which build
 * answered" a recorded fact rather than an assumption.
 *
 * Usage:  php mine_constants.php '["Core","standard",...]'
 *
 * The argument is the extension allowlist (the function catalog's own set); an
 * empty array keeps everything the build has. Output is JSON on stdout; the
 * classification, the version ranges and the committed table are all the
 * xtask's business (`cargo xtask mine-constants`).
 *
 * A float is emitted as its `var_export` SPELLING, not as a JSON number:
 * `PHP_FLOAT_MAX` and friends are `INF`/`NAN`-adjacent magnitudes that
 * `json_encode` either refuses or rounds, and a rounded `M_PI` is a wrong
 * answer rather than a missing one.
 *
 * `build` is the second half of the answer, and it is what keeps a PACKAGING
 * difference from reading as a language change. php-src guards whole blocks of
 * constant registrations on the linked library's version and on configure-time
 * features — `LIBXML_NO_XXE` behind `LIBXML_VERSION >= 21300`, `MHASH_*` behind
 * `--with-mhash`, `LDAP_OPT_X_SASL_*` behind `HAVE_LDAP_SASL` — so two builds
 * that differ there disagree about which constants exist for a reason that has
 * nothing to do with their PHP minor. Each fact is read through a name the guard
 * does NOT gate (a version constant, or a function the same flag registers),
 * because probing a guard with the thing it guards answers only itself.
 */

$allow = [];
if ($argc > 1 && $argv[1] !== '') {
    $decoded = json_decode($argv[1], true);
    if (!is_array($decoded)) {
        fwrite(STDERR, "argument 1 must be a JSON array of extension names\n");
        exit(1);
    }
    $allow = $decoded;
}

$grouped = get_defined_constants(true);
$rows = [];
$extensions = [];
$skipped = [];
$total = 0;

foreach ($grouped as $ext => $constants) {
    // `user` is whatever the mining script itself defined — never an engine fact.
    if ($ext === 'user') {
        continue;
    }
    if ($allow !== [] && !in_array($ext, $allow, true)) {
        $skipped[$ext] = count($constants);
        continue;
    }
    $extensions[] = $ext;
    foreach ($constants as $name => $value) {
        $total++;
        $row = ['ext' => $ext];
        if (is_int($value)) {
            $row['type'] = 'int';
            $row['value'] = (string) $value;
        } elseif (is_float($value)) {
            if (!is_finite($value)) {
                // `INF` and `NAN`. Both are spec-fixed and neither is a value
                // the analyzer can carry: the value domain's float inhabitant is
                // ordered and compared, and `NAN != NAN` would make a singleton
                // fact that disagrees with itself. Recorded as refused rather
                // than dropped.
                $row['type'] = 'unrepresentable';
                $row['value'] = 'non-finite float';
            } else {
                $row['type'] = 'float';
                $row['value'] = var_export($value, true);
            }
        } elseif (is_bool($value)) {
            $row['type'] = 'bool';
            $row['value'] = $value ? 'true' : 'false';
        } elseif ($value === null) {
            $row['type'] = 'null';
            $row['value'] = 'null';
        } elseif (is_string($value)) {
            // The BYTES, base64'd: a constant's value is a PHP string, which is
            // a byte string (ADR-0080), and JSON only carries text.
            $row['type'] = 'string';
            $row['value'] = base64_encode($value);
        } else {
            // Arrays and objects: no constant of the mined extensions has one,
            // and a row that cannot be spelled is recorded as refused rather
            // than dropped.
            $row['type'] = 'unrepresentable';
            $row['value'] = gettype($value);
        }
        $rows[$name] = $row;
    }
}

sort($extensions);

/**
 * The BUILD facts a minor boundary is checked against, `fact => spelling`.
 *
 * `absent` for a fact this build cannot answer — that is itself a difference
 * from a build that can, which is the reading the caller wants.
 */
$constant = static fn (string $name): string => defined($name) ? (string) constant($name) : 'absent';
$build = [
    // Which packager built this engine. An `until` is never minted across a
    // change of packager: "the name is gone at the next minor" and "the next
    // minor was built by somebody else" are indistinguishable then, and only one
    // of them is a fact about PHP.
    'packager' => (static function (string $prefix): string {
        if (str_starts_with($prefix, '/nix/store')) {
            return 'nix';
        }
        // `/Cellar/` alone: Homebrew's prefix is `<root>/Cellar/php/<version>`
        // and the root moves (Apple silicon, Intel, Linuxbrew under a home
        // directory), so the segment is the stable part of the classification.
        if (str_contains($prefix, '/Cellar/')) {
            return 'homebrew';
        }
        if (str_starts_with($prefix, '/opt/local')) {
            return 'macports';
        }
        if (str_starts_with($prefix, '/usr')) {
            return 'system';
        }
        return 'other';
    })(PHP_PREFIX),
    // Linked-library versions. Every one of these is a constant the miner itself
    // refuses as build-dependent, which is precisely why it is the right probe.
    'libxml' => $constant('LIBXML_VERSION'),
    'libxslt' => $constant('LIBXSLT_DOTTED_VERSION'),
    'pcre' => $constant('PCRE_VERSION'),
    'zlib' => $constant('ZLIB_VERNUM'),
    'openssl' => $constant('OPENSSL_VERSION_NUMBER'),
    'icu' => $constant('INTL_ICU_VERSION'),
    'gd' => $constant('GD_VERSION'),
    'gmp' => $constant('GMP_VERSION'),
    'sodium' => $constant('SODIUM_LIBRARY_VERSION'),
    'libpq' => $constant('PGSQL_LIBPQ_VERSION'),
    'iconv' => $constant('ICONV_VERSION') . '/' . $constant('ICONV_IMPL'),
    'oniguruma' => $constant('MB_ONIGURUMA_VERSION'),
    'readline' => $constant('READLINE_LIB'),
    'curl' => function_exists('curl_version') ? (string) (curl_version()['version'] ?? '?') : 'absent',
    'libzip' => defined('ZipArchive::LIBZIP_VERSION')
        ? (string) constant('ZipArchive::LIBZIP_VERSION')
        : 'absent',
    'sqlite' => class_exists('SQLite3') ? (string) SQLite3::version()['versionString'] : 'absent',
    // Configure-time FEATURES, each read through a function the same flag
    // registers alongside the constants it gates: `--with-mhash` brings
    // `mhash()` with `MHASH_*`, and `HAVE_LDAP_SASL` brings `ldap_sasl_bind()`
    // with `LDAP_OPT_X_SASL_*`.
    'mhash_bc' => function_exists('mhash') ? 'yes' : 'no',
    'ldap_sasl' => function_exists('ldap_sasl_bind') ? 'yes' : 'no',
];

echo json_encode([
    'php' => PHP_VERSION,
    'build' => $build,
    'extensions' => $extensions,
    'skipped_extensions' => $skipped,
    'constants_total' => $total,
    'rows' => $rows,
], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES), "\n";
