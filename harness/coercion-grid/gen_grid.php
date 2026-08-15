<?php
// Emit the Steins-side view of `witness.php`'s grid: every (parameter type ×
// witness value) cell as a real call site, one per line, so
// `steins check --profile strict` can be eyeballed against the `php -r` answers
// by hand. The automated cell-for-cell comparison is
// `crates/steins-infer/tests/coercion_witness_grid.rs`, which builds the same
// sources in memory and reads the same `.tsv` files; this script exists for the
// human loop, and for regenerating a fixture when a divergence needs a name.
//
// Usage: php gen_grid.php strict|coercive > grid-<mode>.php
//
// A Steins finding on a line whose trailing comment says `accept` is a false
// positive; a missing finding on a `TypeError` line is a silence (which may be
// deliberate — see the README's divergence list).

$mode = $argv[1] ?? 'coercive';
$decl = $mode === 'strict' ? "declare(strict_types=1);\n" : '';

// Keys are the parameter-type spellings `witness.php` uses; values are the
// signature and a PHP-identifier-safe suffix for the generated function name.
$params = [
    'int'          => ['int $v', 'int'],
    'float'        => ['float $v', 'float'],
    'string'       => ['string $v', 'string'],
    'bool'         => ['bool $v', 'bool'],
    '?int'         => ['?int $v', 'nint'],
    'int|string'   => ['int|string $v', 'intstr'],
    'string|false' => ['string|false $v', 'strfalse'],
    'DateTime'     => ['\DateTime $v', 'dt'],
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

$expected = [];
$tsv = __DIR__ . "/witness-{$mode}.tsv";
if (is_readable($tsv)) {
    foreach (file($tsv, FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES) as $line) {
        $c = explode("\t", $line);
        $expected[$c[1] . "\0" . $c[2]] = $c[4];
    }
}

$out = "<?php\n{$decl}\n";
foreach ($params as $pname => [$sig, $suffix]) {
    $out .= "function p_{$suffix}({$sig}): void {}\n";
}
$out .= "\n";
foreach ($params as $pname => [$sig, $suffix]) {
    foreach ($values as $vname => $vlit) {
        $verdict = $expected[$pname . "\0" . $vname] ?? '?';
        $out .= "p_{$suffix}({$vlit}); // param={$pname} value={$vname} php={$verdict}\n";
    }
}
echo $out;
