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
// be folded (the ADR-0008 allowlist). The runner's sole jobs are: call the
// named builtin with positional literal args, and report the outcome as one of
// value / throw / widen. It must never crash: any misuse widens.

// Keep stdout pure NDJSON — divert any warning/notice/deprecation text to
// stderr (which the parent discards) so it can never corrupt a response line.
ini_set('display_errors', 'stderr');
ini_set('log_errors', '0');

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
        // Documented stub (ADR-0024): the seam exists, the behavior does not yet.
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
        } catch (\Throwable $e) {
            $return_type = null;
            $tentative = false;
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
    ];
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
    $args = isset($params['args']) && is_array($params['args']) ? $params['args'] : [];

    if (!is_string($fn) || !function_exists($fn)) {
        return ['kind' => 'widen', 'reason' => 'unknown function'];
    }

    // Positional args only — never named.
    $args = array_values($args);

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
        // Arrays are OK if they round-trip cleanly (no objects/resources/binary
        // strings inside). The Rust IR has no array literal yet, so the caller
        // will widen anyway — but reporting it faithfully keeps the protocol
        // honest for when arrays arrive.
        $encoded = json_encode($v, JSON_PRESERVE_ZERO_FRACTION);
        if ($encoded === false) {
            return ['kind' => 'widen', 'reason' => 'unencodable array'];
        }
        return ['kind' => 'value', 'value' => $v, 'type' => 'array'];
    }

    // Objects, resources, closures: not a literal we carry.
    return ['kind' => 'widen', 'reason' => 'unencodable type'];
}
