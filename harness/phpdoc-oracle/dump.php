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
 * Tag mode (`--tags`, issue #932): each input line is one docblock tag
 * (`@var Foo $x the result`), run through the full PhpDocParser, and the verdict
 * is where the tag put its type and its variable:
 *   TAG\t<type>\t<variable>   a typed tag; <variable> is empty when it names none
 *   INVALID                  the tag value did not parse (an InvalidTagValueNode)
 *   OTHER\t<class>            a tag value this mode does not read
 * A typeless `@param $x` reads as `TAG` with an empty type.
 *
 * Usage:
 *   php dump.php < inputs.txt
 *   php dump.php inputs.txt
 *   php dump.php --tags inputs.txt
 */

require __DIR__ . '/vendor/autoload.php';

use PHPStan\PhpDocParser\Lexer\Lexer;
use PHPStan\PhpDocParser\Parser\ConstExprParser;
use PHPStan\PhpDocParser\Parser\TokenIterator;
use PHPStan\PhpDocParser\Parser\TypeParser;
use PHPStan\PhpDocParser\Parser\PhpDocParser;
use PHPStan\PhpDocParser\ParserConfig;
use PHPStan\PhpDocParser\Ast\PhpDoc;

$config = new ParserConfig([]);
$lexer = new Lexer($config);
$typeParser = new TypeParser($config, new ConstExprParser($config));

$args = array_slice($argv, 1);
$tagMode = in_array('--tags', $args, true);
$args = array_values(array_filter($args, static fn (string $a): bool => $a !== '--tags'));
$phpDocParser = $tagMode
    ? new PhpDocParser($config, $typeParser, new ConstExprParser($config))
    : null;

$argvFile = $args[0] ?? null;
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
    echo $phpDocParser !== null
        ? dumpTag($lexer, $phpDocParser, $input)
        : dumpOne($lexer, $typeParser, $input), "\n";
}

/** Tag mode: one `@tag value` line through the full docblock parser. */
function dumpTag(Lexer $lexer, PhpDocParser $parser, string $input): string
{
    $node = $parser->parse(new TokenIterator($lexer->tokenize('/** ' . $input . ' */')));
    $tags = $node->getTags();
    if (count($tags) !== 1) {
        return 'OTHER' . "\t" . 'tags=' . count($tags);
    }
    $v = $tags[0]->value;
    if ($v instanceof PhpDoc\InvalidTagValueNode) {
        return 'INVALID';
    }
    [$type, $variable] = match (true) {
        $v instanceof PhpDoc\VarTagValueNode => [(string) $v->type, $v->variableName],
        $v instanceof PhpDoc\ParamTagValueNode => [(string) $v->type, $v->parameterName],
        $v instanceof PhpDoc\TypelessParamTagValueNode => ['', $v->parameterName],
        $v instanceof PhpDoc\PropertyTagValueNode => [(string) $v->type, $v->propertyName],
        $v instanceof PhpDoc\AssertTagValueNode => [(string) $v->type, $v->parameter],
        default => [null, null],
    };
    if ($type === null) {
        return "OTHER\t" . get_class($v);
    }
    return "TAG\t" . escapeControls($type) . "\t" . $variable;
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
