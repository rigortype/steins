<?php

// Steins PHP sidecar runner (ADR-0004 / ADR-0024).
//
// A single, dependency-free file embedded in the `steins` binary via
// `include_str!` and written to a temp dir at startup. It runs the *project's
// own* PHP: literal folding must yield the value this code produces on the
// runtime it actually runs on.
//
// Wire protocol: JSON-RPC 2.0 with NDJSON framing. One request object per line
// on stdin; one response object per line on stdout, until stdin closes. Only
// `json_encode`/`json_decode` are used, so this runs on any PHP 8.1+ with zero
// composer install. PHP 8.1-compatible syntax throughout.
//
// The runner does NOT enforce purity — the Rust side gates which functions may
// be folded (the ADR-0008 allowlist). The runner's sole jobs are: rebuild the
// positional literal args (scalars, and array literals in the entry form
// `steins_decode_arg` documents), call the named builtin with them, and report
// the outcome as one of value / throw / widen. It must never crash: any misuse
// widens.

// Keep stdout pure NDJSON — divert any warning/notice/deprecation text to
// stderr (which the parent discards) so it can never corrupt a response line.
//
// The routing goes through the ERROR LOG, not `display_errors = 'stderr'`. That
// special value is honored only by the cli/cgi SAPIs: under an `embed` SAPI —
// which is what php-wasm is (issue #64) — it is accepted, round-trips through
// `ini_get`, and is inert, so a notice would land mid-NDJSON on stdout and
// corrupt the response. `log_errors` + `error_log = 'php://stderr'` works on
// both. The only difference on a cli run is a `PHP Warning: ` prefix on a
// stream the parent discards.
ini_set('display_errors', '0');
ini_set('log_errors', '1');
ini_set('error_log', 'php://stderr');

// Bound the memory a single fold may claim. Two purposes, both load-bearing:
//
// 1. Blast radius. An allocation that exhausts `memory_limit` is a FATAL, not a
//    Throwable — `steins_fold`'s catch cannot see it, and the process dies
//    mid-NDJSON taking every later request in the run with it. The limit does
//    not make that fatal catchable; it makes it cheap and quick, and the parent
//    recovers by respawning (see `Sidecar::revive` on the Rust side).
// 2. Host independence. Without this, whether `str_repeat('x', 200000000)`
//    folds depends on the machine's php.ini — the same source would fold here
//    and widen on a colleague's box. Pinning the limit makes the fold outcome a
//    property of the code, not of the host.
//
// 256M is far above anything a legitimate fold needs (array arguments are
// capped at a few hundred entries, and the rest are snippet-sized literals),
// and far below the point where a runaway allocation costs real time. A fold
// that would only succeed above it now fatals, which widens — sound.
ini_set('memory_limit', '256M');

// The fold seam's array budget, mirrored from the Rust constants
// `FOLD_ARRAY_MAX_ENTRIES` / `FOLD_ARRAY_MAX_DEPTH` (ADR-0028's issue-#39
// amendment §4). The result direction charges the SAME numbers — here before
// encoding, and again on arrival — so the gate's verdict and the decoder's are
// one verdict computed twice: a shape admissible as an argument is admissible
// as a result.
//
// This is deliberately NOT the `memory_limit` above. That one prices "the child
// will die allocating this"; this one prices how much value the analysis will
// absorb once it returns. `range(1, 1000000)` passes the first and fails this,
// so a single constant would have to be wrong for one of them.
//
// Declared here rather than beside `steins_charge_array_result` because a
// top-level `const` is executed where it is written, not hoisted like a
// function — one placed after the read loop would never be defined.
const STEINS_FOLD_ARRAY_MAX_ENTRIES = 256;
const STEINS_FOLD_ARRAY_MAX_DEPTH = 8;

$in = fopen('php://stdin', 'r');
$out = fopen('php://stdout', 'w');

while (($line = fgets($in)) !== false) {
    $line = trim($line);
    if ($line === '') {
        continue;
    }

    $req = json_decode($line, true);
    if (!is_array($req)) {
        // Unparseable line: no id to answer with, so skip it silently.
        continue;
    }

    $id = array_key_exists('id', $req) ? $req['id'] : null;
    $method = isset($req['method']) && is_string($req['method']) ? $req['method'] : '';
    $params = isset($req['params']) && is_array($req['params']) ? $req['params'] : [];

    $result = steins_handle($method, $params);

    $resp = ['jsonrpc' => '2.0', 'id' => $id, 'result' => $result];
    $encoded = json_encode(
        $resp,
        JSON_PRESERVE_ZERO_FRACTION | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE
    );
    if ($encoded === false) {
        // A result we could not encode (should not happen — encode_value guards
        // this) still owes a well-formed reply.
        $encoded = json_encode([
            'jsonrpc' => '2.0',
            'id' => $id,
            'result' => ['kind' => 'widen', 'reason' => 'unencodable response'],
        ]);
    }
    fwrite($out, $encoded . "\n");
    fflush($out);
}

/**
 * Dispatch one JSON-RPC method to its handler.
 *
 * @param string $method
 * @param array<mixed> $params
 * @return array<string, mixed>
 */
function steins_handle($method, array $params)
{
    switch ($method) {
        case 'env':
            return steins_env();
        case 'fold':
            return steins_fold($params);
        case 'reflect':
            return steins_reflect($params);
        case 'reflect_class':
            return steins_reflect_class($params);
        case 'preg_compile':
            return steins_preg_compile($params);
        case 'defined':
            return steins_defined($params);
        // Documented stub (ADR-0024), and it stays one. The plugin channel's
        // first facts (issue #68) arrive by a MANIFEST the Rust side reads
        // directly — `vendor/<name>/steins-plugin.json`, carrying label
        // registrations and function colorings — because those are static per
        // installed version, and reading them here would make discovery depend
        // on a working `php`. This method remains for the half a manifest cannot
        // answer (ADR-0039): booting the project's own autoload and asking the
        // real framework for synthetic declarations.
        case 'plugin':
            return ['kind' => 'widen', 'reason' => 'unimplemented'];
        default:
            return ['kind' => 'widen', 'reason' => 'unknown method'];
    }
}

/**
 * `env` — coverage-posture material (ADR-0024).
 *
 * @return array<string, mixed>
 */
function steins_env()
{
    return [
        'php_version' => PHP_VERSION,
        'extensions' => array_values(get_loaded_extensions()),
        'sapi' => PHP_SAPI,
        // The engine's INTEGER WIDTH in bytes (issue #64). A version string does
        // not determine the integer machine: php-wasm 0.1.0 is PHP 8.5.2 built
        // 32-bit, where `1 << 40` is 0, `crc32()` goes negative and `hexdec()`
        // promotes to float. Same minor, different arithmetic — so the Rust side
        // gates the fold lane on this, not on the minor alone.
        'int_size' => PHP_INT_SIZE,
    ];
}

/**
 * `reflect` — the runtime boot-surface existence oracle (ADR-0024 / ADR-0049 §1
 * oracle (b)). Answers whether the *project's own* PHP knows a name among its
 * builtins and loaded extensions, as a function and/or a class-like (class,
 * interface, trait, or enum). Autoload is disabled: the sidecar runs no project
 * autoloader, and the question is strictly "is this name resident on this PHP".
 *
 * The reply is always `{kind: "reflection", ...}` — a name that exists nowhere is a
 * *structured not-found* (`exists: false`), never a `widen`; only a malformed
 * request widens. The Rust side maps a widen/malformed reply to "unknown" (None),
 * so a not-found is a positive answer, never confused with a failed query.
 *
 * ## Return-type surface (ADR-0056 R1)
 *
 * When the name is a resident function, the reply also carries its **native
 * return type** as read off the running engine's own arginfo: `return_type` is
 * the `(string)` rendering of `ReflectionFunction::getReturnType()` (e.g.
 * `"bool"`, `"int"`, `"?string"`, `"int|false"`), or `null` when the function
 * declares none. A function that declares no return type but a *tentative* one
 * (`ReflectionFunction::getTentativeReturnType()`, still the engine's own claim
 * for its own builtin) reports that instead, with `return_type_tentative: true`
 * so the consumer can treat it distinctly if it ever needs to. Both render
 * through the SAME `(string)` cast — one wire form — and the boolean tag is the
 * only distinction (ADR-0056 §7 open question, resolved here). By-ref out-param
 * types are NOT surfaced in v1 (the value-domain seed is the ordinary return
 * only). Any reflection failure leaves `return_type` null — never a guess.
 *
 * ## Arity surface (ADR-0064's mixed-pin ruling)
 *
 * Alongside the return type, a resident function reports its **parameter counts**:
 * `params_total` is `ReflectionFunction::getNumberOfParameters()` and
 * `params_required` is `getNumberOfRequiredParameters()`. They exist because a
 * declared return type of `mixed` pins nothing — the array read-position family
 * (`current`, `array_pop`, …) all declare `mixed` — so a structural transfer rule
 * written against such a name countersigns itself against the live *signature*
 * instead: the engine must still take the one argument the rule was written for.
 * (The same surface is what `call.too-many-arguments` for internal targets has
 * been waiting on; this reply carries it, no checker consumes it yet.)
 *
 * Both counts sit inside the same try/catch as the return type, so a reflection
 * failure leaves them `null` — an absent count is unanswerable, never a guess, and
 * a consumer that cannot read the arity withholds its rule exactly as it withholds
 * on an absent declaration.
 *
 * @param array<mixed> $params
 * @return array<string, mixed>
 */
function steins_reflect(array $params)
{
    $target = isset($params['target']) && is_string($params['target']) ? $params['target'] : null;
    if ($target === null) {
        return ['kind' => 'widen', 'reason' => 'reflect requires a string target'];
    }

    // PHP resolves `\Foo` and `Foo` to the same symbol; the existence functions
    // want the leading backslash stripped.
    $name = ltrim($target, '\\');

    $function = function_exists($name);
    // A class-like is any of class / interface / trait / enum. `enum_exists` is
    // guarded for defensiveness though it is present on every PHP 8.1+ the runner
    // supports. Autoload is off (second arg `false`) throughout.
    $class_like = class_exists($name, false)
        || interface_exists($name, false)
        || trait_exists($name, false)
        || (function_exists('enum_exists') && enum_exists($name, false));

    // The native return-type surface (ADR-0056). Only for resident functions;
    // never crash — a reflection failure yields a null return type (widen-safe).
    $return_type = null;
    $tentative = false;
    $params_total = null;
    $params_required = null;
    if ($function) {
        try {
            $rf = new ReflectionFunction($name);
            $rt = $rf->getReturnType();
            if ($rt === null && method_exists($rf, 'getTentativeReturnType')) {
                $tt = $rf->getTentativeReturnType();
                if ($tt !== null) {
                    $rt = $tt;
                    $tentative = true;
                }
            }
            if ($rt !== null) {
                $return_type = (string) $rt;
            }
            $params_total = $rf->getNumberOfParameters();
            $params_required = $rf->getNumberOfRequiredParameters();
        } catch (\Throwable $e) {
            $return_type = null;
            $tentative = false;
            $params_total = null;
            $params_required = null;
        }
    }

    return [
        'kind' => 'reflection',
        'target' => $target,
        'exists' => $function || $class_like,
        'function' => $function,
        'class_like' => $class_like,
        'return_type' => $return_type,
        'return_type_tentative' => $tentative,
        'params_total' => $params_total,
        'params_required' => $params_required,
    ];
}

/**
 * `reflect_class` — the *declaration* behind a resident class-like (issue #269).
 *
 * `reflect` above answers existence; this answers WHAT. It is the class-world half
 * of ADR-0024's `reflect(target)` surface: a class an installed extension provides
 * (`Redis`, `Collator`, `SQLite3`, `Dom\Element`) has no source declaration and no
 * builtin-catalog row, so today it is Unknown everywhere. The project's own PHP has
 * the whole declaration and is the only honest source for it (ADR-0049 §1: ask the
 * real thing, never a curated stub list).
 *
 * ## No autoload, ever
 *
 * Existence is settled first with the `*_exists($name, false)` family, so
 * `ReflectionClass` is only ever constructed for a class the engine ALREADY has
 * resident. The sidecar boots no project autoloader (ADR-0024), and a query that
 * would have to run project code to be answered is not answered at all.
 *
 * ## What travels, and what does not
 *
 * Methods carry the signature surface a consumer can check against — name, the
 * static/abstract/final flags, visibility, the two parameter counts, and the
 * `(string)` rendering of the declared (or tentative) return type, exactly as
 * `reflect` renders a function's. Constants and properties travel as declarations,
 * NOT as values: `getReflectionConstants()` is used rather than `getConstants()`
 * precisely so no constant *initializer is evaluated* inside the sidecar. Interface
 * names are the transitive set `ReflectionClass::getInterfaceNames()` reports; the
 * parent is the direct one.
 *
 * A reflection failure is a `widen` — unanswerable — never a half-filled
 * declaration: a consumer must not be able to mistake "we could not read the members"
 * for "the class has none".
 *
 * @param array<mixed> $params
 * @return array<string, mixed>
 */
function steins_reflect_class(array $params)
{
    $target = isset($params['target']) && is_string($params['target']) ? $params['target'] : null;
    if ($target === null || $target === '') {
        return ['kind' => 'widen', 'reason' => 'reflect_class requires a non-empty string target'];
    }

    $name = ltrim($target, '\\');

    $is_interface = interface_exists($name, false);
    $is_trait = trait_exists($name, false);
    $is_enum = function_exists('enum_exists') && enum_exists($name, false);
    $is_class = class_exists($name, false);
    if (!$is_interface && !$is_trait && !$is_enum && !$is_class) {
        // A structured not-found, exactly as `reflect` spells one: the engine
        // genuinely does not have this class-like.
        return ['kind' => 'class_reflection', 'target' => $target, 'exists' => false];
    }

    try {
        $rc = new ReflectionClass($name);

        if ($is_enum) {
            $class_kind = 'enum';
        } elseif ($is_interface) {
            $class_kind = 'interface';
        } elseif ($is_trait) {
            $class_kind = 'trait';
        } else {
            $class_kind = 'class';
        }

        $parent = $rc->getParentClass();
        $extension = $rc->getExtensionName();

        $methods = [];
        foreach ($rc->getMethods() as $m) {
            $return_type = null;
            $tentative = false;
            $rt = $m->getReturnType();
            if ($rt === null && method_exists($m, 'getTentativeReturnType')) {
                $tt = $m->getTentativeReturnType();
                if ($tt !== null) {
                    $rt = $tt;
                    $tentative = true;
                }
            }
            if ($rt !== null) {
                $return_type = (string) $rt;
            }
            $methods[] = [
                'name' => $m->getName(),
                'static' => $m->isStatic(),
                'abstract' => $m->isAbstract(),
                'final' => $m->isFinal(),
                'visibility' => steins_visibility($m),
                'params_total' => $m->getNumberOfParameters(),
                'params_required' => $m->getNumberOfRequiredParameters(),
                'return_type' => $return_type,
                'return_type_tentative' => $tentative,
            ];
        }

        $constants = [];
        foreach ($rc->getReflectionConstants() as $c) {
            $constants[] = ['name' => $c->getName(), 'visibility' => steins_visibility($c)];
        }

        $properties = [];
        foreach ($rc->getProperties() as $p) {
            $properties[] = [
                'name' => $p->getName(),
                'static' => $p->isStatic(),
                'visibility' => steins_visibility($p),
            ];
        }

        return [
            'kind' => 'class_reflection',
            'target' => $target,
            'exists' => true,
            'name' => $rc->getName(),
            'class_kind' => $class_kind,
            'internal' => $rc->isInternal(),
            'extension' => $extension === false ? null : $extension,
            'final' => $rc->isFinal(),
            'abstract' => $rc->isAbstract(),
            'parent' => $parent === false ? null : $parent->getName(),
            'interfaces' => array_values($rc->getInterfaceNames()),
            'methods' => $methods,
            'constants' => $constants,
            'properties' => $properties,
        ];
    } catch (\Throwable $e) {
        // Unanswerable, never a partial declaration: a consumer must not read a
        // failed read as an empty class.
        return ['kind' => 'widen', 'reason' => 'class reflection failed'];
    }
}

/**
 * `public` / `protected` / `private` for any reflection member that reports the
 * three predicates (methods, class constants, properties).
 *
 * @param ReflectionMethod|ReflectionClassConstant|ReflectionProperty $member
 * @return string
 */
function steins_visibility($member)
{
    if ($member->isPrivate()) {
        return 'private';
    }
    if ($member->isProtected()) {
        return 'protected';
    }
    return 'public';
}

/**
 * `preg_compile` — does THIS engine's PCRE accept the pattern? (issue #189 /
 * ADR-0078, ADR-0004's ask-the-real-thing.)
 *
 * The Rust side's pattern reader decides that a pattern is a proven literal worth
 * asking about; it never decides whether PCRE accepts it. A reader-derived refusal
 * would report patterns PCRE compiles happily, which the zero-FP bar forbids — so
 * the refusal comes from here, from the project's own engine and its own PCRE
 * build, or it does not come at all.
 *
 * ## The probe, and why it is safe
 *
 * `@preg_match($pattern, '')`: compilation is unavoidable, and the *match* runs
 * against a ZERO-LENGTH subject, so there is exactly one start position and no
 * input to backtrack over. Measured at PHP 8.5.9: the textbook catastrophic
 * pattern `/(a+)+$/` answers in 0.0001s on `''` (and only hits the backtrack limit
 * once given a real adversarial subject), and a pattern that IS expensive to
 * compile fails fast (`/(?:a){1,100000}/` → `number too big in {} quantifier`,
 * 0.0000s). A pathological pattern that still burns time inside the engine hits
 * PCRE's own backtrack/recursion/JIT-stack limits, and past those the transport's
 * per-request timeout and `memory_limit` bound it from outside. Nothing here can
 * run the caller's pattern against the caller's data: the subject is `''`, always.
 *
 * ## What carries the compile message — and what does NOT
 *
 * Measured at PHP 8.5.9, `@preg_match('/(unclosed/', '')`:
 *
 * * `preg_last_error_msg()` is **`"Internal error"`** — the PREG error *category*,
 *   with none of PCRE's diagnosis. It is the obvious candidate and it is useless.
 * * `error_get_last()['message']` is
 *   `preg_match(): Compilation failed: missing closing parenthesis at offset 9` —
 *   PCRE's own words, at `E_WARNING` severity (type 2), and `@` does not stop it
 *   being recorded. This is what travels.
 *
 * The message is prefixed with the name of the function that *ran*, and PHP uses
 * the real call site's name (`preg_split(): Compilation failed: …` at a
 * `preg_split` site). Our probe always says `preg_match`, so the prefix is stripped
 * here and the consumer re-attaches its own site's name — otherwise a `preg_split`
 * finding would quote a warning naming `preg_match`, which the engine never emits.
 *
 * ## Why a `false` return alone is NOT a refusal
 *
 * `@preg_match('/(?R)/', '')` returns `false` with `preg_last_error()` =
 * `PREG_JIT_STACKLIMIT_ERROR` and NO recorded diagnostic: the pattern compiled
 * fine and the *match* hit a runtime limit. Reporting it would be a false positive
 * of exactly the kind this whole detour exists to avoid. So a refusal requires all
 * three: a `false` return, `preg_last_error() === PREG_INTERNAL_ERROR` (the
 * category compile failures land in), and a freshly-recorded diagnostic to quote.
 * Anything else widens.
 *
 * @param array<mixed> $params
 * @return array<string, mixed>
 */
function steins_preg_compile(array $params)
{
    $pattern = isset($params['pattern']) && is_string($params['pattern']) ? $params['pattern'] : null;
    if ($pattern === null) {
        return ['kind' => 'widen', 'reason' => 'preg_compile requires a string pattern'];
    }

    // Clear first: `error_get_last()` is process-wide and survives across requests,
    // so an unrelated notice from an earlier fold would otherwise be quoted as this
    // pattern's compile error.
    error_clear_last();
    try {
        $matched = @preg_match($pattern, '');
    } catch (\Throwable $e) {
        // Nothing observed throws here (a non-string pattern cannot reach this
        // point), but the runner's standing contract is that any misuse widens.
        return ['kind' => 'widen', 'reason' => 'preg_match threw'];
    }

    if ($matched !== false) {
        return ['kind' => 'preg', 'status' => 'compiles'];
    }

    // `false` with any other PREG category is a RUNTIME limit on a pattern that
    // compiled — see the `(?R)` witness above.
    if (preg_last_error() !== PREG_INTERNAL_ERROR) {
        return ['kind' => 'widen', 'reason' => 'not a compile refusal'];
    }

    $last = error_get_last();
    $message = is_array($last) && isset($last['message']) && is_string($last['message'])
        ? $last['message']
        : '';
    if ($message === '') {
        return ['kind' => 'widen', 'reason' => 'no diagnostic recorded'];
    }

    // Strip our probe's own `preg_match(): ` prefix; the consumer re-attaches the
    // name of the function that actually appears at the call site.
    $prefix = 'preg_match(): ';
    if (strncmp($message, $prefix, strlen($prefix)) === 0) {
        $message = substr($message, strlen($prefix));
    }

    return ['kind' => 'preg', 'status' => 'refuses', 'message' => $message];
}

/**
 * `defined` — does this engine have the global constant `$name`? (issue #198)
 *
 * The `constant.undefined` ladder's last leg. A curated list can never answer it:
 * the constant a loaded extension provides is a property of the engine actually
 * running the project, so the engine is asked (ADR-0049 §1, ADR-0004).
 *
 * ## Why the name is screened before `defined()` sees it
 *
 * `defined('C::K')` is a *class*-constant query, and PHP will **autoload** `C` to
 * answer it — running project code inside the sidecar, which this process must
 * never do. The caller only ever asks about bare global constants, so a name
 * carrying `::` is a protocol violation and widens rather than being asked.
 *
 * `defined()` itself neither autoloads nor throws for a plain name, and it is
 * case-sensitive on the constant's final segment (case-insensitive constants died
 * with the third argument to `define()` in PHP 8.0), so the name travels verbatim.
 *
 * @param array<mixed> $params
 * @return array<string, mixed>
 */
function steins_defined(array $params)
{
    $name = isset($params['name']) && is_string($params['name']) ? $params['name'] : null;
    if ($name === null || $name === '') {
        return ['kind' => 'widen', 'reason' => 'defined requires a non-empty string name'];
    }
    if (strpos($name, '::') !== false) {
        return ['kind' => 'widen', 'reason' => 'class constants are not asked here'];
    }

    $name = ltrim($name, '\\');
    return ['kind' => 'constant', 'status' => defined($name) ? 'defined' : 'not_defined'];
}

/**
 * `fold` — execute one builtin call over positional literal args.
 *
 * @param array<mixed> $params
 * @return array<string, mixed>
 */
function steins_fold(array $params)
{
    $fn = isset($params['function']) ? $params['function'] : null;
    $raw = isset($params['args']) && is_array($params['args']) ? $params['args'] : [];

    if (!is_string($fn) || !function_exists($fn)) {
        return ['kind' => 'widen', 'reason' => 'unknown function'];
    }

    // Positional args only — never named.
    //
    // Decoding gets its OWN catch, and it is not the call's. Rebuilding an array
    // literal runs PHP's own key rules (`$arr[] =` for an absent key), and those
    // rules can THROW: `[PHP_INT_MAX => 'a', 'b']` raises "Cannot add element to
    // the array as the next element is already occupied". That is a fact about the
    // argument, not a result of the folded call, so it widens rather than
    // reporting `kind => throw` — and it must be caught here rather than left to
    // escape, because an uncaught Error is a FATAL that takes the resident runner
    // down mid-NDJSON and with it every later request in the run. The runner's
    // standing contract is that any misuse widens.
    //
    // The threshold is the engine's own `PHP_INT_MAX`, so on a 32-bit build
    // (php-wasm, issue #64) it is 2147483647 — well inside what the fold seam's
    // width guard admits, and a key a human plausibly writes.
    try {
        $decoded = steins_decode_args(array_values($raw));
    } catch (\Throwable $e) {
        return ['kind' => 'widen', 'reason' => 'undecodable argument'];
    }
    if ($decoded === null) {
        return ['kind' => 'widen', 'reason' => 'undecodable argument'];
    }
    $args = $decoded;

    try {
        $ret = $fn(...$args);
    } catch (\ArgumentCountError $e) {
        // Arity mismatch is a structural misuse, not a value-domain result.
        return ['kind' => 'widen', 'reason' => 'wrong arity'];
    } catch (\Throwable $e) {
        // Any other Throwable is a *result*, not an error (ADR-0024): folding
        // `1/0` reports DivisionByZeroError as type information.
        return ['kind' => 'throw', 'class' => get_class($e)];
    }

    return steins_encode_value($ret);
}

/**
 * Decode the wire form of a positional argument list (issue #39).
 *
 * @param array<mixed> $args
 * @return array<int, mixed>|null the decoded args, or null when any is malformed
 */
function steins_decode_args(array $args)
{
    $out = [];
    foreach ($args as $a) {
        $one = steins_decode_arg($a);
        if ($one === null) {
            return null;
        }
        $out[] = $one[0];
    }
    return $out;
}

/**
 * Decode one wire argument.
 *
 * A scalar arrives bare (`5`, `"x"`, `true`, `null`); an array arrives as
 * `{"__steins_array": [[key, value], ...]}` where `key` is null for an absent key,
 * or an int/string for an explicit one. Values recurse, so nested array literals
 * decode too.
 *
 * **The key rules are PHP's, on purpose** (ADR-0004): an absent key is appended
 * with `$arr[] =`, so this engine's own next-int rule assigns it — including the
 * negative-key edge PHP 8.3 changed — and a repeated key is a plain assignment,
 * so duplicates resolve by this engine's own last-wins. Nothing here reimplements
 * array semantics; that is the entire reason folding runs on the project's PHP.
 *
 * The return is wrapped in a one-element array so that a successfully decoded
 * `null` value is distinguishable from a decode failure (which returns null).
 *
 * @param mixed $a
 * @return array{0: mixed}|null
 */
function steins_decode_arg($a)
{
    if (is_int($a) || is_float($a) || is_string($a) || is_bool($a) || $a === null) {
        return [$a];
    }
    if (!is_array($a) || !array_key_exists('__steins_array', $a) || !is_array($a['__steins_array'])) {
        return null;
    }
    $arr = [];
    foreach ($a['__steins_array'] as $entry) {
        if (!is_array($entry) || !array_key_exists(0, $entry) || !array_key_exists(1, $entry)) {
            return null;
        }
        $value = steins_decode_arg($entry[1]);
        if ($value === null) {
            return null;
        }
        $key = $entry[0];
        if ($key === null) {
            $arr[] = $value[0];
        } elseif (is_int($key) || is_string($key)) {
            $arr[$key] = $value[0];
        } else {
            return null;
        }
    }
    return [$arr];
}

/**
 * Encode a PHP return value as a typed fold result, or widen when it cannot
 * round-trip through JSON cleanly.
 *
 * @param mixed $v
 * @return array<string, mixed>
 */
function steins_encode_value($v)
{
    if (is_int($v)) {
        return ['kind' => 'value', 'value' => $v, 'type' => 'int'];
    }
    if (is_float($v)) {
        // NaN / INF have no JSON spelling and no literal in our IR.
        if (!is_finite($v)) {
            return ['kind' => 'widen', 'reason' => 'non-finite float'];
        }
        return ['kind' => 'value', 'value' => $v, 'type' => 'float'];
    }
    if (is_string($v)) {
        // Only valid UTF-8 survives JSON; binary strings widen.
        if (json_encode($v) === false) {
            return ['kind' => 'widen', 'reason' => 'non-utf8 string'];
        }
        return ['kind' => 'value', 'value' => $v, 'type' => 'string'];
    }
    if (is_bool($v)) {
        return ['kind' => 'value', 'value' => $v, 'type' => 'bool'];
    }
    if ($v === null) {
        return ['kind' => 'value', 'value' => null, 'type' => 'null'];
    }
    if (is_array($v)) {
        // An array *result* crosses the seam since the ADR-0028 amendment of
        // 2026-08-14 (issue #330), in the same `__steins_array` envelope the
        // argument direction uses — see `steins_decode_arg`. The budget is
        // charged and every leaf validated BEFORE the envelope is built, so an
        // oversized or unencodable answer never becomes a megabyte of JSON.
        $budget = STEINS_FOLD_ARRAY_MAX_ENTRIES;
        $reason = steins_charge_array_result($v, STEINS_FOLD_ARRAY_MAX_DEPTH, $budget);
        if ($reason !== null) {
            return ['kind' => 'widen', 'reason' => $reason];
        }
        return ['kind' => 'value', 'value' => steins_encode_array($v), 'type' => 'array'];
    }

    // Objects, resources, closures: not a literal we carry.
    return ['kind' => 'widen', 'reason' => 'unencodable type'];
}

/**
 * Charge an array result against the budget and validate every key and leaf in
 * the same recursive pass, returning a widen reason or null when it fits.
 *
 * One bad leaf anywhere widens the WHOLE result: a partial array is a *wrong*
 * value, not a wider one, and widening is the only safe direction (ADR-0002).
 * The depth bound is checked on entry, so this function's own recursion is
 * bounded by it — the same property that keeps the two encoders off an
 * unbounded stack.
 *
 * `$budget` is by-reference because entries are counted **recursively**: a
 * nested array's entries spend the same allowance its parent's do.
 *
 * @param array<mixed> $v
 * @param int $depth
 * @param int $budget
 * @return string|null the widen reason, or null when the result may be encoded
 */
function steins_charge_array_result(array $v, $depth, &$budget)
{
    if ($depth === 0) {
        return 'array result over depth budget';
    }
    foreach ($v as $key => $item) {
        if ($budget === 0) {
            return 'array result over entry budget';
        }
        $budget--;
        // A binary string KEY would fail the response encode just as a binary
        // string value would, so it is validated on the same footing.
        if (is_string($key) && json_encode($key) === false) {
            return 'non-utf8 string';
        }
        if (is_array($item)) {
            $nested = steins_charge_array_result($item, $depth - 1, $budget);
            if ($nested !== null) {
                return $nested;
            }
            continue;
        }
        $leaf = steins_charge_leaf($item);
        if ($leaf !== null) {
            return $leaf;
        }
    }
    return null;
}

/**
 * Validate one non-array leaf of an array result, returning a widen reason or
 * null. The reasons are the scalar encoder's own, so a `"\xC0"` inside an array
 * widens for the same stated cause as a `"\xC0"` returned bare — which is what
 * lets ADR-0080 §3.1 lift both with one tagged-bytes variant later.
 *
 * @param mixed $v
 * @return string|null
 */
function steins_charge_leaf($v)
{
    if (is_string($v)) {
        return json_encode($v) === false ? 'non-utf8 string' : null;
    }
    if (is_float($v)) {
        return is_finite($v) ? null : 'non-finite float';
    }
    if (is_int($v) || is_bool($v) || $v === null) {
        return null;
    }
    // Objects, resources, closures inside the array: no literal in the IR.
    return 'unencodable type';
}

/**
 * Encode an already-charged array as the `__steins_array` envelope: an ordered
 * entry list `[[key, value], ...]` whose keys are the MATERIALIZED int/string
 * keys PHP has already assigned.
 *
 * A result therefore never spells an absent key — PHP finished building this
 * array, so next-int assignment and last-wins have already happened here, in
 * the engine, where ADR-0004 wants them. The decoder rejects a `null` key for
 * exactly that reason rather than re-deriving a next-int Rust did not choose.
 *
 * Scalars encode bare (float-ness survives via the response's
 * `JSON_PRESERVE_ZERO_FRACTION`); nested arrays nest their own envelopes.
 *
 * @param array<mixed> $v
 * @return array<string, mixed>
 */
function steins_encode_array(array $v)
{
    $entries = [];
    foreach ($v as $key => $item) {
        $entries[] = [$key, is_array($item) ? steins_encode_array($item) : $item];
    }
    return ['__steins_array' => $entries];
}
