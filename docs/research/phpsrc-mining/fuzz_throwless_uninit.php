<?php
// Run-time witness for docs/research/phpsrc-mining/throwless_audit.md (issue #881).
//
// A static walk cannot resolve a call through an object handler pointer
// (`Z_OBJ_HT_P(o)->cast_object(...)`), and an engine handler can raise for an object
// that no constructor initialised. This calls every name the audit lists with an
// uninitialised instance of a user subclass of every extensible internal class
// (ReflectionClass::newInstanceWithoutConstructor), directly and as the only element
// of an array, at every position the reach table (crates/steins-catalog/src/reach.rs)
// calls `Inert` (a declared scalar type, or a curated `OVERRIDES` row): a position
// anywhere else is a reach the resolver turns into a gap of its own. The other
// positions take values from a small benign pool.
//
//   php fuzz_throwless_uninit.php            every name, one child process per name
//   php fuzz_throwless_uninit.php <name>     one name
//
// Prints each Throwable that is not an argument check, once per name, class and
// message, with the internal classes that produce it. Run on PHP 8.5.11.
$self = __FILE__;
$note = file_get_contents(__DIR__ . '/throwless_audit.md');
preg_match_all('/^\| `([a-z0-9_]+)` \|/m', $note, $m);
if (!isset($argv[1])) {
    // A handler can end the process with a fatal error: one child per name.
    foreach ($m[1] as $name) passthru(escapeshellarg(PHP_BINARY) . ' -d memory_limit=4G ' . escapeshellarg($self) . ' ' . escapeshellarg($name));
    exit(0);
}
$name = $argv[1];
error_reporting(0);
ob_start(fn($b) => '');
set_error_handler(fn() => true);
$skip = ['restore_error_handler', 'restore_exception_handler', 'error_reporting', 'ini_set',
    'mt_srand', 'srand', 'date_default_timezone_set', 'umask', 'fclose'];
if (in_array($name, $skip, true)) exit(0);

$reach = file_get_contents(__DIR__ . '/../../../crates/steins-catalog/src/reach.rs');
$reach = substr($reach, strpos($reach, 'const OVERRIDES'));
$reach = substr($reach, 0, strpos($reach, '#[cfg(test)]'));
$over = [];
preg_match_all('/\("(\w+)", &\[([^\]]*)\]\)/', $reach, $rows, PREG_SET_ORDER);
foreach ($rows as $row) {
    preg_match_all('/\((\d+), Inert\)/', $row[2], $ps);
    foreach ($ps[1] as $p) $over[$row[1]][(int)$p] = true;
}

$objs = [];
$i = 0;
foreach (get_declared_classes() as $cls) {
    $rc = new ReflectionClass($cls);
    if (!$rc->isInternal() || $rc->isFinal() || $rc->isAbstract() || $rc->isInterface() || $rc->isEnum()) continue;
    $u = 'UX' . $i++;
    try {
        eval("class $u extends \\$cls {}");
        $objs[$cls] = (new ReflectionClass($u))->newInstanceWithoutConstructor();
    } catch (Throwable $e) {}
}
$benign = [0, 1, 'a', [], [1], null, true];
$rf = new ReflectionFunction($name);
$inertTy = [];
foreach ($rf->getParameters() as $pi => $pp) {
    $t = $pp->getType();
    $members = $t instanceof ReflectionUnionType ? $t->getTypes() : ($t ? [$t] : []);
    $inertTy[$pi] = $members && !array_filter($members, fn($x) => !in_array($x->getName(), ['int', 'float', 'bool', 'true', 'false', 'null']));
}
$seen = [];
$max = $rf->isVariadic() ? 3 : min(3, $rf->getNumberOfParameters());
for ($arity = max(1, $rf->getNumberOfRequiredParameters()); $arity <= $max; $arity++) {
    foreach ($objs as $cls => $o) {
        for ($pos = 0; $pos < $arity; $pos++) {
            if (!isset($over[$name][$pos]) && !$inertTy[$pos]) continue;
            foreach ([false, true] as $wrap) {
                $idx = array_fill(0, $arity, 0);
                while (true) {
                    $vals = [];
                    $args = [];
                    foreach ($idx as $k => $bi) { $vals[$k] = $k === $pos ? ($wrap ? [$o] : $o) : $benign[$bi]; $args[$k] = &$vals[$k]; }
                    try { @$name(...$args); }
                    catch (Throwable $e) {
                        $msg = $e->getMessage();
                        $arg = $e instanceof ArgumentCountError
                            || ($e instanceof TypeError && preg_match('/^' . preg_quote($name, '/') . '\(\): Argument #\d+/', $msg))
                            || ($e instanceof Error && str_contains($msg, 'could not be converted to string'));
                        if (!$arg) $seen[$name . ' | ' . get_class($e) . ' | ' . preg_replace('/\d+/', 'N', substr($msg, 0, 80))][$cls] = true;
                    }
                    $k = $arity - 1;
                    while ($k >= 0 && ($k === $pos || ++$idx[$k] >= count($benign))) {
                        if ($k !== $pos) $idx[$k] = 0;
                        $k--;
                    }
                    if ($k < 0) break;
                }
            }
        }
    }
}
ob_end_clean();
foreach ($seen as $key => $classes) {
    echo $key, '  <= ', implode(', ', array_slice(array_keys($classes), 0, 6)), count($classes) > 6 ? ' ...(' . count($classes) . ')' : '', "\n";
}
