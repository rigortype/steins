<?php

declare(strict_types=1);

/**
 * Mine PHPStan's `resources/functionMap.php` (and its per-minor delta files) for
 * builtin RETURN TYPE declarations — the data source of the ADR-0069 Asserted
 * declared-envelope floor (issue #73).
 *
 * The map is a giant PHP array literal with an idiosyncratic key grammar, so it is
 * parsed by PHP itself (ask-the-real-thing, ADR-0004/ADR-0024) rather than by a
 * hand-rolled reader: this script `require`s the map and emits JSON on stdout.
 *
 * Usage:
 *     php docs/research/phpstan-mining/mine_function_map.php /path/to/phpstan-src
 *
 * Key grammar (from the map's own header comment):
 *   'name'            => ['<return type>', 'arg'=>'<type>', ...]
 *   "name'1"          => an ALTERNATE signature for the same function
 *   'Class::method'   => a method row
 *
 * # The delta files are applied FORWARD
 *
 * `functionMap.php` is the map for the OLDEST PHP PHPStan supports, not the
 * newest. `FunctionSignatureMapProvider::computeSignatureMap()` walks it upward:
 * for every supported minor at or below the target, the delta's `old` keys are
 * UNSET and its `new` keys are inserted. This script reproduces that algorithm
 * exactly (see `apply_delta()`), so the emitted rows are the signatures PHPStan
 * itself would use at each minor — not the raw base map.
 *
 * What this script does, and deliberately does not:
 *   - METHOD rows (`::` in the key) are skipped — the floor is function-keyed
 *     (ADR-0056 §6's "method-keyed rows in v1" refusal still stands).
 *   - ALTERNATE signatures are folded into their base name. When every alternate
 *     agrees on the return type the name keeps it; when they disagree the name is
 *     EXCLUDED and listed, because a floor row must state one envelope.
 *   - The DELTA files are also read as a change oracle: a name whose RETURN type
 *     differs between two adjacent supported minors changed at the upper one, and
 *     the boundary is recorded. A name that merely APPEARS at some minor is an
 *     existence fact, which this table never speaks to (ADR-0069 §2).
 *   - `functionMap_bleedingEdge.php` / `functionMap_php80delta_bleedingEdge.php`
 *     are skipped: PHPStan applies them only under `stricterFunctionMap`, an
 *     opt-in stricter-than-the-engine posture, and the floor must state what the
 *     engine states.
 *
 * No filtering by lowerability and no reflection cross-check happens here: both are
 * the Rust generator's job (`cargo xtask gen-function-map`), which owns the same
 * `lower_str` seam the analyzer uses.
 */

$root = $argv[1] ?? null;
if ($root === null || !is_dir($root)) {
    fwrite(STDERR, "usage: php mine_function_map.php /path/to/phpstan-src\n");
    exit(2);
}

$mapPath = $root . '/resources/functionMap.php';
if (!is_file($mapPath)) {
    fwrite(STDERR, "not found: {$mapPath}\n");
    exit(2);
}

/**
 * Split a functionMap key into its base name (the alternate-signature suffix
 * `'1`, `'2`, … dropped).
 */
function base_name(string $key): string
{
    $tick = strpos($key, "'");
    return $tick === false ? $key : substr($key, 0, $tick);
}

/**
 * PHPStan's own delta algorithm, transcribed: unset every `old` key, then insert
 * every `new` key.
 *
 * @param array<string, mixed> $map
 * @param array{new?: array<string, mixed>, old?: array<string, mixed>} $delta
 * @return array<string, mixed>
 */
function apply_delta(array $map, array $delta): array
{
    foreach (array_keys($delta['old'] ?? []) as $key) {
        unset($map[strtolower((string) $key)]);
    }
    foreach ($delta['new'] ?? [] as $key => $signature) {
        $map[strtolower((string) $key)] = $signature;
    }
    return $map;
}

/**
 * Reduce a raw signature map to `name => return type`, folding alternates.
 *
 * @param array<string, mixed> $map
 * @return array{rows: array<string, string>, disagree: array<string, list<string>>, methods: int, malformed: list<string>}
 */
function reduce_map(array $map): array
{
    $methods = 0;
    $malformed = [];
    /** @var array<string, array<string, true>> $byName */
    $byName = [];
    foreach ($map as $key => $signature) {
        $key = (string) $key;
        if (str_contains($key, '::')) {
            $methods++;
            continue;
        }
        if (!is_array($signature) || !isset($signature[0]) || !is_string($signature[0])) {
            $malformed[] = $key;
            continue;
        }
        $byName[strtolower(base_name($key))][$signature[0]] = true;
    }
    $rows = [];
    $disagree = [];
    foreach ($byName as $name => $types) {
        $seen = array_keys($types);
        if (count($seen) === 1) {
            $rows[$name] = $seen[0];
            continue;
        }
        sort($seen);
        $disagree[$name] = $seen;
    }
    ksort($rows);
    ksort($disagree);
    return ['rows' => $rows, 'disagree' => $disagree, 'methods' => $methods, 'malformed' => $malformed];
}

/** @var array<string, mixed> $base */
$base = require $mapPath;
$base = array_change_key_case($base, CASE_LOWER);
$totalKeys = count($base);

// The deltas PHPStan applies, in the order it applies them, each tagged with the
// minor it lifts the map TO. 7.4 and 8.0 are below Steins' floor but must still be
// applied to reach a correct 8.1 map.
$ladder = [
    ['74', [7, 4]],
    ['80', [8, 0]],
    ['81', [8, 1]],
    ['82', [8, 2]],
    ['83', [8, 3]],
    ['84', [8, 4]],
    ['85', [8, 5]],
];

/** @var array<string, array<string, string>> $perMinor "8.1" => rows */
$perMinor = [];
$map = $base;
$reduced = null;
foreach ($ladder as [$tag, $minor]) {
    $path = $root . "/resources/functionMap_php{$tag}delta.php";
    if (!is_file($path)) {
        fwrite(STDERR, "missing delta: {$path}\n");
        exit(2);
    }
    /** @var array{new?: array<string, mixed>, old?: array<string, mixed>} $delta */
    $delta = require $path;
    $map = apply_delta($map, $delta);
    $reduced = reduce_map($map);
    $perMinor[$minor[0] . '.' . $minor[1]] = $reduced['rows'];
}

// The pin: PHP 8.5, the last rung of the ladder. `$reduced` holds its reduction.
assert($reduced !== null);
$pinRows = $reduced['rows'];

// The change oracle: walk the SUPPORTED minors pairwise (8.1 → 8.5). A name whose
// return type differs between two adjacent minors changed at the upper one.
$supported = ['8.1', '8.2', '8.3', '8.4', '8.5'];
/** @var array<string, list<string>> $versionSensitive */
$versionSensitive = [];
for ($i = 1; $i < count($supported); $i++) {
    $lo = $perMinor[$supported[$i - 1]];
    $hi = $perMinor[$supported[$i]];
    foreach ($hi as $name => $type) {
        if (!isset($lo[$name])) {
            continue; // appeared at this minor: existence, not a return-type change
        }
        if ($lo[$name] !== $type) {
            $versionSensitive[$name][] = $supported[$i];
        }
    }
}
ksort($versionSensitive);

echo json_encode([
    'phpstan_src' => $root,
    'php_version' => PHP_VERSION,
    'total_keys' => $totalKeys,
    'methods_skipped' => $reduced['methods'],
    'malformed' => $reduced['malformed'],
    'rows' => $pinRows,
    'alternates_disagree' => $reduced['disagree'],
    'version_sensitive' => $versionSensitive,
], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . "\n";
