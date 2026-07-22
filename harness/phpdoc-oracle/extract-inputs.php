<?php declare(strict_types = 1);

/**
 * Mechanically port phpstan/phpdoc-parser's TypeParserTest data-provider inputs
 * into a flat fixtures file (ADR-0029). We do NOT re-transcribe the corpus by
 * hand: we load the reference test class itself, call its `provideParseData()`
 * provider reflectively, and emit every input type-expression string.
 *
 * The reference TestCase extends PHPUnit\Framework\TestCase; we stub that base
 * class so the file loads without PHPUnit present — the provider never touches
 * `$this`, it only constructs Ast nodes (from the installed vendor) to describe
 * expected results, and we ignore those, keeping only row[0] (the input).
 *
 * Output: one input per line, C-escaped (\\ \n \t \r), to stdout.
 *
 * Usage: php extract-inputs.php > ../../crates/steins-phpdoc/tests/fixtures/reference-types.txt
 */

require __DIR__ . '/vendor/autoload.php';

// Stub the PHPUnit base class the reference test extends, so we can load the
// file without pulling in PHPUnit. Declared only if absent.
if (!class_exists('PHPUnit\\Framework\\TestCase', false)) {
    eval('namespace PHPUnit\\Framework; class TestCase {}');
}

$testFile = __DIR__ . '/reference-tests/TypeParserTest.php';
if (!is_file($testFile)) {
    fwrite(STDERR, "reference test not found: {$testFile}\n");
    exit(2);
}
require $testFile;

$class = 'PHPStan\\PhpDocParser\\Parser\\TypeParserTest';
$ref = new ReflectionClass($class);
$instance = $ref->newInstanceWithoutConstructor();

$rows = $instance->provideParseData();

$seen = [];
$out = [];
foreach ($rows as $row) {
    if (!is_array($row) || !array_key_exists(0, $row)) {
        continue;
    }
    $input = $row[0];
    if (!is_string($input)) {
        continue;
    }
    if (isset($seen[$input])) {
        continue;
    }
    $seen[$input] = true;
    $out[] = cescape($input);
}

foreach ($out as $line) {
    echo $line, "\n";
}

fwrite(STDERR, sprintf("extracted %d unique type-expression inputs from %s\n", count($out), $class));

/**
 * C-style escape so one logical input occupies exactly one physical line.
 * Backslash first, then the whitespace controls. Everything else is literal
 * (including the FQCN backslashes, which become `\\`).
 */
function cescape(string $s): string
{
    $s = str_replace('\\', '\\\\', $s);
    $s = str_replace("\n", '\\n', $s);
    $s = str_replace("\r", '\\r', $s);
    $s = str_replace("\t", '\\t', $s);
    return $s;
}
