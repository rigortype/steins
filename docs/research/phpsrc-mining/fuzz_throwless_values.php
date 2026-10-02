<?php
// Run-time witness for docs/research/phpsrc-mining/throwless_audit.md (issue #881).
//
// Calls every name the audit lists, with every tuple of up to three arguments drawn
// from a pool that adds what a plain argument fuzz lacks: arrays that contain
// themselves by reference, an uninitialised DateTimeZone subclass, an open and a
// closed stream, a lazy object. Reports each Throwable that is not an argument
// check, once per name, class and message. No user code throws, so what is left
// depends on a value.
//
//   php fuzz_throwless_values.php [max arity, default 3]
//
// Names whose call would change the process (the error handler stack, ini settings,
// the seed, the default time zone, the umask, a shared stream) are skipped. Run on
// PHP 8.5.11 for the audit at php-8.5.11; the output for a table name must be empty,
// and the output for a `needs-row` name is the witness of its row.
error_reporting(0);
ob_start(fn($b) => '');
set_error_handler(fn() => true);

class Z extends DateTimeZone { public function __construct() {} }
class Plain {}

$note = file_get_contents(__DIR__ . '/throwless_audit.md');
preg_match_all('/^\| `([a-z0-9_]+)` \|/m', $note, $m);
$names = $m[1];
$skip = array_flip(['restore_error_handler', 'restore_exception_handler', 'error_reporting', 'ini_set',
    'mt_srand', 'srand', 'date_default_timezone_set', 'umask', 'fclose']);
$maxArity = (int)($argv[1] ?? 3);

$r = []; $r[0] = &$r;
$r2 = []; $r2[0] = &$r2;
$mem = fopen('php://memory', 'w+');
$closed = fopen('php://memory', 'w+'); fclose($closed);
$lazy = (new ReflectionClass(Plain::class))->newLazyGhost(function ($o) {});
$pool = [
    0, 1, -1, 2, 5, PHP_INT_MAX, PHP_INT_MIN, NAN, INF, 1.5, '', 'a', "a\0b", 'Y-m-d', '/tmp', '1', ' 1',
    '%s', null, true, false,
    [], [1, 2], ['a' => [1], 'b' => 1], $r, $r2, [$r], [$r2], [$r, $r2], [[1], [2]],
    new stdClass, new ArrayObject([1]), new Z, new DateTime('@0'), new Plain, $lazy, fn() => 1, 'abs',
    $mem, $closed, STDIN,
];
$n = count($pool);
$out = [];
$seen = [];
$calls = 0;
foreach ($names as $name) {
    if (isset($skip[$name])) continue;
    $rf = new ReflectionFunction($name);
    $max = $rf->isVariadic() ? $maxArity : min($maxArity, $rf->getNumberOfParameters());
    for ($arity = $rf->getNumberOfRequiredParameters(); $arity <= $max; $arity++) {
        $idx = array_fill(0, $arity, 0);
        while (true) {
            $vals = [];
            $args = [];
            foreach ($idx as $k => $i) { $vals[$k] = $pool[$i]; $args[$k] = &$vals[$k]; }
            $calls++;
            try { @$name(...$args); }
            catch (Throwable $e) {
                $msg = $e->getMessage();
                $arg = $e instanceof ArgumentCountError
                    || ($e instanceof TypeError && preg_match('/^' . preg_quote($name, '/') . '\(\): (Argument #\d+|If argument)/', $msg))
                    || ($e instanceof Error && str_contains($msg, 'could not be converted to string'))
                    // another function's argument check, reached through a callback name in the pool
                    || ($e instanceof TypeError && preg_match('/^abs\(\): Argument #\d+/', $msg));
                if (!$arg) {
                    $key = $name . ' | ' . get_class($e) . ' | ' . preg_replace('/\d+/', 'N', substr($msg, 0, 70));
                    if (!isset($seen[$key])) { $seen[$key] = true; $out[] = $key; }
                }
            }
            $k = $arity - 1;
            while ($k >= 0 && ++$idx[$k] >= $n) { $idx[$k] = 0; $k--; }
            if ($k < 0) break;
        }
    }
}
ob_end_clean();
echo implode("\n", $out), "\ncalls: $calls\n";
