<?php
// Per-parameter facts for every INTERNAL function the running engine has, read
// off its own arginfo through `ReflectionFunction` (issue #382).
//
// Why the engine and not php-src's stubs: this table exists to be an
// INDEPENDENT source. `out_params` and `invocation_shape` were transcribed from
// the stubs by hand, and a second hand-transcription of the same stubs would
// agree with them by construction — including where both are wrong. The running
// engine's arginfo is the thing PHP itself dispatches on.
//
// Emitted as JSON on stdout; `cargo xtask mine-param-facts` turns it into
// `param_facts.toml`. argv[1] is a JSON array of names the catalog reasons
// about, which get their full parameter spellings kept even when they carry no
// hazard — absence has to be a RECORDED fact for the completeness tests to be
// non-vacuous.
//
// Only `ReflectionFunction`, `get_defined_functions` and `json_encode` are used,
// so this runs on any PHP 8.1+ with zero extensions beyond Core.

$keep = json_decode($argv[1] ?? '[]', true);
if (!is_array($keep)) {
    fwrite(STDERR, "argv[1] must be a JSON array of names\n");
    exit(1);
}
$keep = array_flip(array_map('strtolower', $keep));

/** A parameter's declared type as the engine spells it; `mixed` when untyped. */
function spell(ReflectionParameter $p): string
{
    $t = $p->getType();
    return $t === null ? 'mixed' : (string) $t;
}

/** Whether a declared type can accept a callable — the invocation hazard. */
function is_callable_type(string $spell): bool
{
    $low = strtolower($spell);
    // `callable`, `?callable`, `callable|string`, `Closure`, `?Closure`. A bare
    // `mixed` or `string` position is NOT counted: half the standard library
    // would qualify, and the hazard is a parameter the engine INVOKES, which is
    // what a declared callable type marks.
    return str_contains($low, 'callable') || str_contains($low, 'closure');
}

$rows = [];
$plain = [];
$missing = [];
$internal = get_defined_functions()['internal'];
sort($internal);

foreach ($internal as $name) {
    try {
        $rf = new ReflectionFunction($name);
    } catch (Throwable $e) {
        // A name the engine lists but cannot reflect is recorded, not skipped:
        // a silent drop is exactly the vacuity this table exists to remove.
        $missing[] = $name;
        continue;
    }
    $by_ref = [];
    $callable = [];
    $variadic = [];
    $optional = [];
    $params = [];
    $param_names = [];
    foreach ($rf->getParameters() as $i => $p) {
        $s = spell($p);
        $params[] = $s;
        // The declared NAME as well as the type: a size-shaped `int` parameter
        // ($length, $times) turns an oversized probe into a multi-gigabyte
        // allocation and a PHP fatal, and only the name tells it from an offset.
        $param_names[] = $p->getName();
        if ($p->isPassedByReference()) $by_ref[] = $i;
        if (is_callable_type($s)) $callable[] = $i;
        if ($p->isVariadic()) $variadic[] = $i;
        if ($p->isOptional()) $optional[] = $i;
    }
    $hazard = $by_ref !== [] || $callable !== [] || $variadic !== [];
    if (!$hazard && !isset($keep[strtolower($name)])) {
        // No hazard and nothing downstream reasons about it: the NAME is the
        // whole fact, and it is still recorded so its emptiness is provable.
        $plain[] = $name;
        continue;
    }
    $rows[$name] = [
        'by_ref' => $by_ref,
        'callable' => $callable,
        'variadic' => $variadic,
        'optional' => $optional,
        'params' => $params,
        'param_names' => $param_names,
        'params_required' => $rf->getNumberOfRequiredParameters(),
    ];
}

// Names the catalog reasons about that this build does not have at all — an
// unloaded extension, or a name that never existed. Recorded so a reader can
// tell "no such function here" from "no hazard here".
$absent = [];
foreach (array_keys($keep) as $name) {
    if (!function_exists($name)) $absent[] = $name;
}
sort($absent);

$ext = get_loaded_extensions();
sort($ext);

echo json_encode([
    'php' => PHP_VERSION,
    'extensions' => $ext,
    'internal_total' => count($internal),
    'unreflectable' => $missing,
    'absent' => $absent,
    'rows' => $rows,
    'plain' => $plain,
], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
