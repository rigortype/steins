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

echo json_encode([
    'php' => PHP_VERSION,
    'extensions' => $extensions,
    'skipped_extensions' => $skipped,
    'constants_total' => $total,
    'rows' => $rows,
], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES), "\n";
