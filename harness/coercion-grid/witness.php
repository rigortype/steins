<?php
// The parameter-coercion witness grid: what PHP itself does with one value of
// each base handed to each native parameter type, in both coercion modes.
//
// Usage: php witness.php strict   > witness-strict.tsv
//        php witness.php coercive > witness-coercive.tsv
//
// Each cell is a REAL call: a parameter of type T declared in a file whose
// `declare(strict_types=1)` is the mode under test, handed a value of base B.
// The witness printed is the value's literal spelling, so every row of the table
// is a reproducible one-liner.
//
// The value list is one witness per equivalence class of PHP's own coercion
// behaviour, which is why it is nine rows and not four: `bool` splits because a
// `false` literal member accepts exactly one of `true`/`false`, and `string`
// splits because coercive mode decides on `is_numeric`.
//
// ## The internal grid (ADR-0056 §9.3)
//
// `php witness.php <mode> internal` runs the same nine values against REAL
// internal functions instead of a declared userland `f`, because PHP's coercion
// table is not quite the same on both sides of that line: from 8.1 on, `null`
// into a non-nullable scalar parameter of an internal function is a
// *deprecation* in coercive mode and a TypeError only under strict_types. That
// one cell is the whole reason this grid exists — the rest of the rows are here
// so a second divergence cannot hide behind it.
//
//     php witness.php strict   internal > witness-internal-strict.tsv
//     php witness.php coercive internal > witness-internal-coercive.tsv
//
// The row carries one more column than the userland grid (the function name),
// and the parameter type is the engine's own `getType()` rendering — read from
// reflection here, not written down, so a signature change shows up as a diff.

$mode = $argv[1] ?? 'coercive';
$grid = $argv[2] ?? 'userland';
$dir = sys_get_temp_dir() . '/steins-coercion-grid';
@mkdir($dir);

$params = [
    'int'          => 'int $v',
    'float'        => 'float $v',
    'string'       => 'string $v',
    'bool'         => 'bool $v',
    '?int'         => '?int $v',
    'int|string'   => 'int|string $v',
    'string|false' => 'string|false $v',
    'DateTime'     => '\DateTime $v',
];

$values = [
    'int'                 => '0',
    'float(1.5)'          => '1.5',
    'float(1.0)'          => '1.0',
    'string(numeric)'     => "'5'",
    'string(non-numeric)' => "'abc'",
    'bool(true)'          => 'true',
    'bool(false)'         => 'false',
    'null'                => 'null',
    'array'               => '[]',
];

// The internal grid's targets: three real builtins whose first parameter covers
// a single scalar (`string`), another single scalar (`int`), and a scalar union
// (`int|float`). The parameter type is not written here — it is read off the
// running engine below, so a signature change shows up as a diff rather than as
// a silently stale label. Each is side-effect-free and total on its own domain,
// which is what makes running the real call a safe way to ask.
$internal = ['strlen', 'dechex', 'abs'];

$decl = $mode === 'strict' ? "declare(strict_types=1);\n" : '';

if ($grid === 'internal') {
    $rows = [];
    foreach ($internal as $fn) {
        $rp = (new ReflectionFunction($fn))->getParameters()[0];
        $ptype = (string) $rp->getType();
        foreach ($values as $vname => $vlit) {
            // No userland declaration at all: the call under test IS the internal
            // one, so the boundary PHP applies is the internal boundary.
            $src = "<?php\n{$decl}{$fn}({$vlit});\necho \"OK\\n\";\n";
            $file = $dir . '/case.php';
            file_put_contents($file, $src);
            $out = [];
            $rc = 0;
            exec('php -d error_reporting=E_ALL ' . escapeshellarg($file) . ' 2>&1', $out, $rc);
            $text = implode(' ', $out);
            $ok = str_contains($text, 'OK');
            $verdict = $ok ? 'accept' : 'TypeError';
            $note = '';
            if ($ok && str_contains($text, 'Deprecated')) {
                $note = 'deprecated';
            }
            $rows[] = [$mode, $fn, $ptype, $vname, $vlit, $verdict, $note];
        }
    }
    foreach ($rows as $r) {
        echo rtrim(implode("\t", $r), "\t"), "\n";
    }
    exit(0);
}

$rows = [];
foreach ($params as $pname => $psig) {
    foreach ($values as $vname => $vlit) {
        $src = "<?php\n{$decl}function f({$psig}): void {}\nf({$vlit});\necho \"OK\\n\";\n";
        $file = $dir . '/case.php';
        file_put_contents($file, $src);
        $out = [];
        $rc = 0;
        exec('php -d error_reporting=E_ALL ' . escapeshellarg($file) . ' 2>&1', $out, $rc);
        $text = implode(' ', $out);
        $ok = str_contains($text, 'OK');
        $verdict = $ok ? 'accept' : 'TypeError';
        $note = '';
        if ($ok && str_contains($text, 'Deprecated')) {
            $note = 'deprecated';
        }
        $rows[] = [$mode, $pname, $vname, $vlit, $verdict, $note];
    }
}
foreach ($rows as $r) {
    echo rtrim(implode("\t", $r), "\t"), "\n";
}
