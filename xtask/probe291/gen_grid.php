<?php
// Generate the Steins-side grid fixture: every (source base × parameter type)
// pair as a real call site, so `steins check` can be diffed against the
// `php -r` witness grid produced by witness.php.
//
// Usage: php gen_grid.php strict|coercive > grid-<mode>.php

$mode = $argv[1] ?? 'coercive';
$decl = $mode === 'strict' ? "declare(strict_types=1);\n" : '';

$params = [
    'int'          => 'int $v',
    'float'        => 'float $v',
    'string'       => 'string $v',
    'bool'         => 'bool $v',
    'nint'         => '?int $v',
    'intstr'       => 'int|string $v',
    'strfalse'     => 'string|false $v',
    'dt'           => '\DateTime $v',
];
$sources = ['int' => 'int', 'float' => 'float', 'string' => 'string', 'bool' => 'bool'];

$out = "<?php\n{$decl}\n";
foreach ($params as $key => $sig) {
    $out .= "function p_{$key}({$sig}): void {}\n";
}
$out .= "\n";
foreach ($sources as $skey => $stype) {
    $out .= "function src_{$skey}({$stype} \$s): void {\n";
    foreach ($params as $pkey => $_) {
        $out .= "    p_{$pkey}(\$s); // src={$skey} param={$pkey}\n";
    }
    $out .= "}\n";
}
echo $out;
