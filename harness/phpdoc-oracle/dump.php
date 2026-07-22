<?php declare(strict_types = 1);

/**
 * The differential oracle (ADR-0029): run the *real* phpstan/phpdoc-parser over
 * type-expression strings and emit its verdict per input. This is the
 * compatibility reference that steins-phpdoc is checked against — in the ported
 * fixtures test and in `cargo xtask phpdoc-oracle` over the corpus.
 *
 * Input (one type-expression per line, C-escaped `\\ \n \t \r`) on stdin or a
 * file argument. Blank lines and lines beginning with `#` are ignored so the
 * fixtures file can carry a header/comments.
 *
 * Output, one result line per input line, tab-separated:
 *   OK\t<canonical>       parsed a type, entire input consumed (nextToken=END)
 *   PARTIAL\t<canonical>  parsed a type, but trailing tokens remain
 *   ERROR\t<message>      the parser threw (reference rejects the input)
 * <canonical> is the node's `__toString()` — phpdoc-parser's own canonical form,
 * with literal newlines/tabs escaped so the result stays single-line.
 *
 * Usage:
 *   php dump.php < inputs.txt
 *   php dump.php inputs.txt
 */

require __DIR__ . '/vendor/autoload.php';

use PHPStan\PhpDocParser\Lexer\Lexer;
use PHPStan\PhpDocParser\Parser\ConstExprParser;
use PHPStan\PhpDocParser\Parser\TokenIterator;
use PHPStan\PhpDocParser\Parser\TypeParser;
use PHPStan\PhpDocParser\ParserConfig;

$config = new ParserConfig([]);
$lexer = new Lexer($config);
$typeParser = new TypeParser($config, new ConstExprParser($config));

$argvFile = $argv[1] ?? null;
$handle = $argvFile !== null ? fopen($argvFile, 'r') : STDIN;
if ($handle === false) {
    fwrite(STDERR, "cannot open input: {$argvFile}\n");
    exit(2);
}

while (($line = fgets($handle)) !== false) {
    $line = rtrim($line, "\n\r");
    // Header/comment/blank lines in the fixtures file are passed through as-is
    // so `.txt` and `.expected` stay line-aligned.
    if ($line === '' || $line[0] === '#') {
        echo $line, "\n";
        continue;
    }
    $input = cunescape($line);
    echo dumpOne($lexer, $typeParser, $input), "\n";
}

function dumpOne(Lexer $lexer, TypeParser $typeParser, string $input): string
{
    try {
        $tokens = new TokenIterator($lexer->tokenize($input));
        $node = $typeParser->parse($tokens);
        $canonical = escapeControls((string) $node);
        $atEnd = $tokens->currentTokenType() === Lexer::TOKEN_END;
        return ($atEnd ? "OK\t" : "PARTIAL\t") . $canonical;
    } catch (\Throwable $e) {
        return "ERROR\t" . escapeControls($e->getMessage());
    }
}

/** Inverse of the extractor's C-escaping. */
function cunescape(string $s): string
{
    $out = '';
    $len = strlen($s);
    for ($i = 0; $i < $len; $i++) {
        $c = $s[$i];
        if ($c === '\\' && $i + 1 < $len) {
            $n = $s[$i + 1];
            $i++;
            switch ($n) {
                case 'n': $out .= "\n"; break;
                case 'r': $out .= "\r"; break;
                case 't': $out .= "\t"; break;
                case '\\': $out .= '\\'; break;
                default: $out .= '\\' . $n; break;
            }
        } else {
            $out .= $c;
        }
    }
    return $out;
}

/** Keep a result single-line: escape any literal control chars in the output. */
function escapeControls(string $s): string
{
    return strtr($s, ["\\" => '\\\\', "\n" => '\\n', "\r" => '\\r', "\t" => '\\t']);
}
