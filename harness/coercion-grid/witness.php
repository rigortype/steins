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

$mode = $argv[1] ?? 'coercive';
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

$decl = $mode === 'strict' ? "declare(strict_types=1);\n" : '';
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
