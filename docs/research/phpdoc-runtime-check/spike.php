<?php declare(strict_types=1);
// Feasibility spike: (1) check runtime values against PHPDoc types using phpstan/phpdoc-parser only,
// (2) derive generators from @param types and run @return as a property (PBT). No analyzer involved.
// Needs only phpstan/phpdoc-parser: `composer require phpstan/phpdoc-parser` next to this file, or point
// PHPDOC_PARSER_AUTOLOAD at any vendor/autoload.php that has it (e.g. a phpstan-src checkout).
require getenv('PHPDOC_PARSER_AUTOLOAD') ?: __DIR__ . '/vendor/autoload.php';

use PHPStan\PhpDocParser\Ast\Type as T;
use PHPStan\PhpDocParser\Ast\ConstExpr as C;
use PHPStan\PhpDocParser\Ast\PhpDoc\PhpDocNode;
use PHPStan\PhpDocParser\Lexer\Lexer;
use PHPStan\PhpDocParser\Parser\{ConstExprParser, PhpDocParser, TokenIterator, TypeParser};
use PHPStan\PhpDocParser\ParserConfig;

enum V: string { case Yes = 'yes'; case No = 'no'; case Unknowable = 'unknowable'; }

final class Verdict {
    public function __construct(public readonly V $v, public readonly string $why = '') {}
    public static function yes(): self { return new self(V::Yes); }
    public static function no(string $why): self { return new self(V::No, $why); }
    public static function unk(string $why): self { return new self(V::Unknowable, $why); }
}

// ============================================================ checker (observation) ==========
final class Checker {
    /** @param array<string, mixed> $params @param array<string, T\TypeNode> $bindings */
    public function __construct(private array $params = [], public array $bindings = []) {}

    public function check(T\TypeNode $t, mixed $x, string $path = '$'): Verdict {
        return match (true) {
            $t instanceof T\IdentifierTypeNode => $this->ident($t->name, $x, $path),
            $t instanceof T\NullableTypeNode => $x === null ? Verdict::yes() : $this->check($t->type, $x, $path),
            $t instanceof T\UnionTypeNode => $this->union($t->types, $x, $path),
            $t instanceof T\IntersectionTypeNode => $this->inter($t->types, $x, $path),
            $t instanceof T\ConstTypeNode => $this->literal($t->constExpr, $x, $path),
            $t instanceof T\GenericTypeNode => $this->generic($t, $x, $path),
            $t instanceof T\ArrayTypeNode => $this->generic(new T\GenericTypeNode(new T\IdentifierTypeNode('array'), [$t->type]), $x, $path),
            $t instanceof T\ArrayShapeNode => $this->shape($t, $x, $path),
            $t instanceof T\ConditionalTypeForParameterNode => $this->condParam($t, $x, $path),
            $t instanceof T\CallableTypeNode => is_callable($x) ? Verdict::unk("{$path}: callable signature not checkable at runtime") : Verdict::no("{$path}: not callable"),
            $t instanceof T\OffsetAccessTypeNode => Verdict::unk("{$path}: offset-access type needs static resolution"),
            $t instanceof T\ObjectShapeNode => $this->objectShape($t, $x, $path),
            default => Verdict::unk("{$path}: " . $t::class . ' not interpreted'),
        };
    }

    private function ident(string $n, mixed $x, string $p): Verdict {
        if (isset($this->bindings[$n])) return $this->check($this->bindings[$n], $x, $p);
        $ok = match ($n) {
            'mixed' => true,
            'int', 'integer' => is_int($x), 'float', 'double' => is_float($x), 'string' => is_string($x), 'bool', 'boolean' => is_bool($x),
            'true' => $x === true, 'false' => $x === false, 'null' => $x === null,
            'array' => is_array($x), 'list' => is_array($x) && array_is_list($x),
            'object' => is_object($x), 'iterable' => is_iterable($x), 'callable' => is_callable($x),
            'scalar' => is_scalar($x), 'array-key' => is_int($x) || is_string($x),
            'positive-int' => is_int($x) && $x > 0, 'negative-int' => is_int($x) && $x < 0,
            'non-empty-string' => is_string($x) && $x !== '',
            'non-falsy-string', 'truthy-string' => is_string($x) && (bool) $x,
            'numeric-string' => is_string($x) && is_numeric($x),
            'lowercase-string' => is_string($x) && strtolower($x) === $x,
            'uppercase-string' => is_string($x) && strtoupper($x) === $x,
            'uncased-string' => is_string($x) && strtolower($x) === strtoupper($x), // Steins vocabulary: parsed as an identifier, interpreted by name
            'non-empty-array' => is_array($x) && $x !== [],
            'non-empty-list' => is_array($x) && $x !== [] && array_is_list($x),
            'class-string' => is_string($x) && (class_exists($x) || interface_exists($x) || enum_exists($x)),
            'literal-string' => null, // not observable at runtime
            'never', 'void' => false,
            default => null,
        };
        if ($ok === null) {
            if (class_exists($n) || interface_exists($n) || enum_exists($n)) return $x instanceof $n ? Verdict::yes() : Verdict::no("{$p}: " . get_debug_type($x) . " is not an instance of {$n}");
            return Verdict::unk("{$p}: '{$n}' has no runtime interpretation");
        }
        return $ok ? Verdict::yes() : Verdict::no("{$p}: " . var_export($x, true) . " is not {$n}");
    }

    private function union(array $arms, mixed $x, string $p): Verdict {
        $unk = [];
        foreach ($arms as $a) { $v = $this->check($a, $x, $p); if ($v->v === V::Yes) return $v; if ($v->v === V::Unknowable) $unk[] = $v->why; }
        return $unk ? Verdict::unk(implode('; ', $unk)) : Verdict::no("{$p}: " . var_export($x, true) . " matches no union arm");
    }

    private function inter(array $arms, mixed $x, string $p): Verdict {
        $unk = [];
        foreach ($arms as $a) { $v = $this->check($a, $x, $p); if ($v->v === V::No) return $v; if ($v->v === V::Unknowable) $unk[] = $v->why; }
        return $unk ? Verdict::unk(implode('; ', $unk)) : Verdict::yes();
    }

    public static function constValue(C\ConstExprNode $c): mixed {
        return match (true) {
            $c instanceof C\ConstExprStringNode => $c->value,
            $c instanceof C\ConstExprIntegerNode => (int) $c->value,
            $c instanceof C\ConstExprFloatNode => (float) $c->value,
            $c instanceof C\ConstExprTrueNode => true, $c instanceof C\ConstExprFalseNode => false, $c instanceof C\ConstExprNullNode => null,
            $c instanceof C\ConstFetchNode => constant(($c->className !== '' ? $c->className . '::' : '') . $c->name),
            default => throw new RuntimeException('unhandled const expr'),
        };
    }

    private function literal(C\ConstExprNode $c, mixed $x, string $p): Verdict {
        $want = self::constValue($c);
        return $x === $want ? Verdict::yes() : Verdict::no("{$p}: " . var_export($x, true) . ' !== ' . var_export($want, true));
    }

    private function generic(T\GenericTypeNode $g, mixed $x, string $p): Verdict {
        $name = $g->type->name; $args = $g->genericTypes;
        switch ($name) {
            case 'int':
                if (!is_int($x)) return Verdict::no("{$p}: not int");
                [$lo, $hi] = $args; $loV = self::bound($lo, PHP_INT_MIN); $hiV = self::bound($hi, PHP_INT_MAX);
                return ($x >= $loV && $x <= $hiV) ? Verdict::yes() : Verdict::no("{$p}: {$x} outside int<{$loV}, {$hiV}>");
            case 'class-string':
                if (!is_string($x) || !(class_exists($x) || interface_exists($x) || enum_exists($x))) return Verdict::no("{$p}: not a class-string");
                $b = $args[0]; $ref = $b instanceof T\IdentifierTypeNode ? ($this->bindings[$b->name] ?? $b) : $b;
                return $ref instanceof T\IdentifierTypeNode && is_a($x, $ref->name, true) ? Verdict::yes() : Verdict::no("{$p}: {$x} is not a subclass of " . $ref->name);
            case 'key-of':
                if ($args[0] instanceof T\ArrayShapeNode) { foreach ($args[0]->items as $it) if (self::keyName($it) === $x) return Verdict::yes(); return Verdict::no("{$p}: not a key of the shape"); }
                return Verdict::unk("{$p}: key-of over a non-literal operand");
            case 'array': case 'list': case 'non-empty-array': case 'non-empty-list': case 'iterable':
                $base = $this->ident($name, $x, $p); if ($base->v !== V::Yes) return $base;
                if ($x instanceof Traversable && !$x instanceof IteratorAggregate && !$x instanceof ArrayIterator) return Verdict::unk("{$p}: consuming a Traversable is destructive; not sampled");
                [$kT, $vT] = count($args) === 2 ? $args : [null, $args[0]];
                foreach ($x as $k => $v) {
                    $kp = var_export($k, true);
                    if ($kT !== null) { $r = $this->check($kT, $k, "{$p}[key {$kp}]"); if ($r->v !== V::Yes) return $r; }
                    $r = $this->check($vT, $v, "{$p}[{$kp}]"); if ($r->v !== V::Yes) return $r;
                }
                return Verdict::yes();
            default:
                $base = $this->ident($name, $x, $p); if ($base->v !== V::Yes) return $base;
                return Verdict::unk("{$p}: type arguments of {$name} are erased at runtime (not a native collection)");
        }
    }

    public static function bound(T\TypeNode $b, int $default): int {
        if ($b instanceof T\ConstTypeNode && $b->constExpr instanceof C\ConstExprIntegerNode) return (int) $b->constExpr->value;
        return $default; // 'min' / 'max'
    }

    public static function keyName(T\ArrayShapeItemNode $it): int|string|null {
        $k = $it->keyName;
        return match (true) { $k instanceof T\IdentifierTypeNode => $k->name, $k instanceof C\ConstExprStringNode => $k->value, $k instanceof C\ConstExprIntegerNode => (int) $k->value, default => null };
    }

    private function shape(T\ArrayShapeNode $s, mixed $x, string $p): Verdict {
        if (!is_array($x)) return Verdict::no("{$p}: not an array");
        if ($s->kind === T\ArrayShapeNode::KIND_LIST && !array_is_list($x)) return Verdict::no("{$p}: not a list");
        $seen = []; $i = 0;
        foreach ($s->items as $it) {
            $k = self::keyName($it) ?? $i++; $seen[$k] = true; $kp = var_export($k, true);
            if (!array_key_exists($k, $x)) { if ($it->optional) continue; return Verdict::no("{$p}: missing key {$kp}"); }
            $r = $this->check($it->valueType, $x[$k], "{$p}[{$kp}]"); if ($r->v !== V::Yes) return $r;
        }
        $extra = array_diff_key($x, $seen);
        if ($extra === []) return Verdict::yes();
        if ($s->sealed) return Verdict::no("{$p}: unexpected keys " . implode(', ', array_map(fn($k) => var_export($k, true), array_keys($extra))));
        $u = $s->unsealedType; if ($u === null) return Verdict::yes();
        foreach ($extra as $k => $v) {
            $kp = var_export($k, true);
            if ($u->keyType !== null) { $r = $this->check($u->keyType, $k, "{$p}[key {$kp}]"); if ($r->v !== V::Yes) return $r; }
            $r = $this->check($u->valueType, $v, "{$p}[{$kp}]"); if ($r->v !== V::Yes) return $r;
        }
        return Verdict::yes();
    }

    private function objectShape(T\ObjectShapeNode $s, mixed $x, string $p): Verdict {
        if (!is_object($x)) return Verdict::no("{$p}: not an object");
        foreach ($s->items as $it) {
            $k = $it->keyName instanceof T\IdentifierTypeNode ? $it->keyName->name : $it->keyName->value;
            if (!property_exists($x, $k)) { if ($it->optional) continue; return Verdict::no("{$p}: missing property {$k}"); }
            $r = $this->check($it->valueType, $x->$k, "{$p}->{$k}"); if ($r->v !== V::Yes) return $r;
        }
        return Verdict::yes();
    }

    /** `($k is int ? A : B)` is decidable at runtime: the argument value is in hand. */
    private function condParam(T\ConditionalTypeForParameterNode $c, mixed $x, string $p): Verdict {
        $name = ltrim($c->parameterName, '$');
        if (!array_key_exists($name, $this->params)) return Verdict::unk("{$p}: no value for \${$name}");
        $test = $this->check($c->targetType, $this->params[$name], "\${$name}");
        if ($test->v === V::Unknowable) return $test;
        $holds = ($test->v === V::Yes) xor $c->negated;
        return $this->check($holds ? $c->if : $c->else, $x, $p . ($holds ? ' [then]' : ' [else]'));
    }
}

// ============================================================ generator (PBT) ================
final class Ungeneratable extends RuntimeException {}

final class Gen {
    /** @param array<string, T\TypeNode> $bindings template name → concrete type chosen for this run */
    public function __construct(public array $bindings = [], private ?Random\Randomizer $rng = null) { $this->rng ??= new Random\Randomizer(new Random\Engine\Mt19937(0)); }
    private function int(int $lo, int $hi): int { return $this->rng->getInt(min($lo, $hi), max($lo, $hi)); }
    private function pick(array $xs): mixed { return $xs[$this->rng->pickArrayKeys($xs, 1)[0]]; }

    public function gen(T\TypeNode $t, int $size): mixed {
        return match (true) {
            $t instanceof T\IdentifierTypeNode => $this->ident($t->name, $size),
            $t instanceof T\NullableTypeNode => $this->int(0, 3) === 0 ? null : $this->gen($t->type, $size),
            $t instanceof T\UnionTypeNode => $this->gen($this->pick($t->types), $size),
            $t instanceof T\ConstTypeNode => Checker::constValue($t->constExpr),
            $t instanceof T\GenericTypeNode => $this->generic($t, $size),
            $t instanceof T\ArrayTypeNode => $this->generic(new T\GenericTypeNode(new T\IdentifierTypeNode('array'), [$t->type]), $size),
            $t instanceof T\ArrayShapeNode => $this->shape($t, $size),
            default => throw new Ungeneratable('no generator for ' . $t::class),
        };
    }

    private function str(int $min, int $max, string $alphabet = 'abcXYZ019 _-'): string {
        $n = $this->int($min, max($min, $max)); $s = '';
        for ($i = 0; $i < $n; $i++) $s .= $alphabet[$this->int(0, strlen($alphabet) - 1)];
        return $s;
    }

    private function ident(string $n, int $size): mixed {
        if (isset($this->bindings[$n])) return $this->gen($this->bindings[$n], $size);
        return match ($n) {
            'int', 'integer' => $this->int(-$size, $size),
            'positive-int' => $this->int(1, max(1, $size)), 'negative-int' => -$this->int(1, max(1, $size)),
            'float', 'double' => $this->int(-$size, $size) / 3,
            'string' => $this->str(0, $size), 'non-empty-string' => $this->str(1, $size),
            'lowercase-string' => strtolower($this->str(0, $size)), 'numeric-string' => (string) $this->int(-$size, $size),
            'bool', 'boolean' => (bool) $this->int(0, 1), 'true' => true, 'false' => false, 'null' => null,
            'array-key' => $this->int(0, 1) ? $this->int(-$size, $size) : $this->str(0, $size),
            'scalar' => $this->gen(new T\UnionTypeNode([new T\IdentifierTypeNode('int'), new T\IdentifierTypeNode('string'), new T\IdentifierTypeNode('bool'), new T\IdentifierTypeNode('float')]), $size),
            'mixed' => $this->gen(new T\UnionTypeNode([new T\IdentifierTypeNode('scalar'), new T\IdentifierTypeNode('null'), new T\IdentifierTypeNode('array')]), $size),
            'array', 'list' => $this->generic(new T\GenericTypeNode(new T\IdentifierTypeNode($n), [new T\IdentifierTypeNode('mixed')]), $size),
            default => $this->object($n, $size),
        };
    }

    private function object(string $n, int $size): mixed {
        if (enum_exists($n)) { $cases = $n::cases(); return $this->pick($cases); }
        if (class_exists($n) && (new ReflectionClass($n))->isInstantiable() && (new ReflectionClass($n))->getConstructor()?->getNumberOfRequiredParameters() === 0) return new $n();
        if ($n === 'object') return new stdClass();
        throw new Ungeneratable("cannot construct '{$n}'");
    }

    private function generic(T\GenericTypeNode $g, int $size): mixed {
        $name = $g->type->name; $args = $g->genericTypes;
        switch ($name) {
            case 'int': return $this->int(max(Checker::bound($args[0], PHP_INT_MIN), -$size), min(Checker::bound($args[1], PHP_INT_MAX), $size));
            case 'list': case 'non-empty-list': case 'array': case 'non-empty-array': case 'iterable':
                $min = str_starts_with($name, 'non-empty') ? 1 : 0; $n = $this->int($min, max($min, $size));
                [$kT, $vT] = count($args) === 2 ? $args : [null, $args[0]]; $out = [];
                for ($i = 0; $i < $n; $i++) { $k = $kT === null || in_array($name, ['list', 'non-empty-list'], true) ? $i : $this->gen($kT, $size); $out[$k] = $this->gen($vT, $size); }
                return $out;
            case 'key-of':
                if ($args[0] instanceof T\ArrayShapeNode) { $keys = array_map(Checker::keyName(...), $args[0]->items); return $this->pick($keys); }
                throw new Ungeneratable('key-of over a non-literal operand');
            default: throw new Ungeneratable("no generator for {$name}<…>");
        }
    }

    private function shape(T\ArrayShapeNode $s, int $size): array {
        $out = []; $i = 0;
        foreach ($s->items as $it) { $k = Checker::keyName($it) ?? $i++; if ($it->optional && $this->int(0, 1)) continue; $out[$k] = $this->gen($it->valueType, $size); }
        return $out;
    }

    /** Candidates simpler than $v (for shrinking); the caller re-validates them against the @param type. */
    public static function shrink(mixed $v): array {
        if (is_int($v)) return array_values(array_unique(array_filter([0, intdiv($v, 2), $v - ($v <=> 0)], fn($c) => $c !== $v)));
        if (is_string($v)) return $v === '' ? [] : [substr($v, 1), substr($v, 0, -1), substr($v, 0, intdiv(strlen($v), 2))];
        if (is_array($v)) { $out = []; foreach (array_keys($v) as $k) { $c = $v; unset($c[$k]); $out[] = array_is_list($v) ? array_values($c) : $c; foreach (self::shrink($v[$k]) as $e) { $c = $v; $c[$k] = $e; $out[] = $c; } } return $out; }
        return [];
    }
}

// ============================================================ docblock → checks / properties ==
final class Doc {
    private static ?PhpDocParser $parser = null; private static ?Lexer $lexer = null;
    private static function p(): array {
        if (self::$parser === null) { $cfg = new ParserConfig(usedAttributes: []); self::$lexer = new Lexer($cfg); $ce = new ConstExprParser($cfg); $tp = new TypeParser($cfg, $ce); self::$parser = new PhpDocParser($cfg, $tp, $ce); }
        return [self::$parser, self::$lexer];
    }
    public static function type(string $s): T\TypeNode { [$p, $l] = self::p(); return $p->parse(new TokenIterator($l->tokenize('/** @var ' . $s . ' $x */')))->getVarTagValues()[0]->type; }
    public static function of(ReflectionFunctionAbstract $rf): PhpDocNode { [$p, $l] = self::p(); return $p->parse(new TokenIterator($l->tokenize($rf->getDocComment() ?: '/** */'))); }

    /** Template bindings from actual argument values (observation) — the runtime kind of the value. */
    public static function bindFromValues(PhpDocNode $doc, array $params): array {
        $b = [];
        foreach ($doc->getTemplateTagValues() as $tpl) foreach ($doc->getParamTagValues() as $pt)
            if ($pt->type instanceof T\IdentifierTypeNode && $pt->type->name === $tpl->name && array_key_exists($n = ltrim($pt->parameterName, '$'), $params)) {
                $v = $params[$n]; $b[$tpl->name] = new T\IdentifierTypeNode(is_object($v) ? $v::class : get_debug_type($v));
            }
        return $b;
    }

    /** OBSERVATION: call $fn with real args, then check every @param and the @return against what flowed. */
    public static function observe(string|Closure $fn, mixed ...$args): array {
        $rf = new ReflectionFunction($fn); $doc = self::of($rf);
        $names = array_map(fn($p) => $p->getName(), $rf->getParameters());
        $params = array_combine(array_slice($names, 0, count($args)), $args);
        $chk = new Checker($params, self::bindFromValues($doc, $params)); $out = [];
        foreach ($doc->getParamTagValues() as $pt) { $n = ltrim($pt->parameterName, '$'); if (!array_key_exists($n, $params)) continue; $out["@param \${$n}"] = $chk->check($pt->type, $params[$n], "\${$n}"); }
        $ret = $rf->invoke(...$args);
        foreach ($doc->getReturnTagValues() as $rt) $out['@return'] = $chk->check($rt->type, $ret, 'return');
        return $out;
    }

    /** PROPERTY: ∀ args ∈ @param (generated), fn(args) ∈ @return and throws ⊆ @throws. Shrinks the first counterexample. */
    public static function property(string|Closure $fn, int $runs = 200, int $seed = 0): string {
        $rf = new ReflectionFunction($fn); $doc = self::of($rf); $name = $rf->getName();
        $paramTypes = []; foreach ($doc->getParamTagValues() as $pt) $paramTypes[ltrim($pt->parameterName, '$')] = $pt->type;
        foreach ($rf->getParameters() as $rp) if (!isset($paramTypes[$rp->getName()])) $paramTypes[$rp->getName()] = Doc::type((string) ($rp->getType() ?? 'mixed')); // native declaration as the fallback envelope
        $throws = array_map(fn($t) => $t->type, $doc->getThrowsTagValues());
        $failing = function (array $args) use ($rf, $doc, $throws): ?string {
            $chk = new Checker($args, self::bindFromValues($doc, $args));
            try { $ret = $rf->invokeArgs($args); }
            catch (Throwable $e) { foreach ($throws as $t) if ($chk->check($t, $e)->v === V::Yes) return null; return 'threw ' . $e::class . ' (' . $e->getMessage() . ') not declared in @throws'; }
            foreach ($doc->getReturnTagValues() as $rt) { $r = $chk->check($rt->type, $ret, 'return'); if ($r->v === V::No) return $r->why . '  (returned ' . json_encode($ret) . ')'; }
            return null;
        };
        for ($i = 1; $i <= $runs; $i++) {
            $gen = new Gen([], new Random\Randomizer(new Random\Engine\Mt19937($seed + $i))); $size = 1 + intdiv($i * 100, $runs); $args = [];
            foreach ($doc->getTemplateTagValues() as $tpl) { $bound = $tpl->bound ?? new T\IdentifierTypeNode('mixed'); $v = $gen->gen($bound, $size); $gen->bindings[$tpl->name] = Doc::type(is_object($v) ? $v::class : get_debug_type($v)); }
            try { foreach ($paramTypes as $n => $t) $args[$n] = $gen->gen($t, $size); }
            catch (Ungeneratable $e) { return "?  {$name}: cannot generate inputs — " . $e->getMessage(); }
            if (($why = $failing($args)) === null) continue;
            // shrink: greedy, only over candidates that still satisfy their @param type
            $steps = 0; $orig = $args;
            shrinking: foreach ($args as $n => $v) foreach (Gen::shrink($v) as $cand) {
                $try = $args; $try[$n] = $cand;
                if ((new Checker($try))->check($paramTypes[$n], $cand)->v !== V::Yes) continue;
                if (($w = $failing($try)) !== null) { $args = $try; $why = $w; $steps++; goto shrinking; }
            }
            $fmt = fn(array $a) => implode(', ', array_map(fn($k, $v) => "\${$k} = " . json_encode($v, JSON_UNESCAPED_UNICODE), array_keys($a), $a));
            return "✗  {$name}: property violated after {$i} run(s) (seed {$seed})\n     counterexample: " . $fmt($args) . "\n     {$why}" . ($steps ? "\n     (shrunk in {$steps} steps from: " . $fmt($orig) . ')' : '');
        }
        return "✓  {$name}: {$runs} runs, @return held for every generated input";
    }
}

// ============================================================ demo ===========================
function show(string $label, Verdict|array $r): void {
    if ($r instanceof Verdict) { printf("%-56s %-10s %s\n", $label, $r->v->value, $r->why); return; }
    foreach ($r as $k => $v) printf("%-56s %-10s %s\n", "{$label} {$k}", $v->v->value, $v->why);
}

echo "== 1. type × value (observation primitive) ==\n";
$c = new Checker();
show("list<non-empty-string> vs ['a','b']", $c->check(Doc::type('list<non-empty-string>'), ['a', 'b']));
show("list<non-empty-string> vs ['a','']", $c->check(Doc::type('list<non-empty-string>'), ['a', '']));
show("array{id: int, name?: string} vs ['id'=>1]", $c->check(Doc::type('array{id: int, name?: string}'), ['id' => 1]));
show("array{id: int} vs ['id'=>1,'x'=>2] (sealed)", $c->check(Doc::type('array{id: int}'), ['id' => 1, 'x' => 2]));
show("array{id: int, ...<string, bool>} vs +x=>true", $c->check(Doc::type('array{id: int, ...<string, bool>}'), ['id' => 1, 'x' => true]));
show("int<0, max> vs -1", $c->check(Doc::type('int<0, max>'), -1));
show("class-string<Throwable> vs RuntimeException", $c->check(Doc::type('class-string<Throwable>'), RuntimeException::class));
show("key-of<array{a: int, b: string}> vs 'b'", $c->check(Doc::type('key-of<array{a: int, b: string}>'), 'b'));
show("uncased-string (Steins vocabulary) vs '123'", $c->check(Doc::type('uncased-string'), '123'));
show("literal-string vs 'x'", $c->check(Doc::type('literal-string'), 'x'));
show("ArrayObject<int, string> vs new ArrayObject()", $c->check(Doc::type('ArrayObject<int, string>'), new ArrayObject()));
show("callable(int): string vs 'strlen'", $c->check(Doc::type('callable(int): string'), 'strlen'));

/**
 * @template T of array-key
 * @param T $k
 * @return ($k is int ? 'int' : 'string')
 */
function kind(int|string $k): string { return is_int($k) ? 'int' : 'string'; }
/** @return ($k is int ? 'int' : 'string') */
function kindBuggy(int|string $k): string { return 'string'; }
/**
 * @template T of object
 * @param T $x
 * @return T
 */
function id(object $x): object { return $x; }
/**
 * @template T of object
 * @param T $x
 * @return T
 */
function idBuggy(object $x): object { return new stdClass(); }
/**
 * @param non-empty-list<int> $xs
 * @return positive-int
 */
function maxOf(array $xs): int { return max($xs); }          // lies for all-negative input
/**
 * @param array{name: non-empty-string, tags?: list<lowercase-string>} $user
 * @return non-empty-string
 */
function label(array $user): string { return $user['name'] . (isset($user['tags']) ? ' #' . implode(' #', $user['tags']) : ''); }
/**
 * @param int<0, 100> $pct
 * @return int<0, 100>
 * @throws InvalidArgumentException
 */
function invert(int $pct): int { if ($pct === 0) throw new LogicException('zero'); return 100 - $pct; } // wrong exception class

echo "\n== 2. observation: real calls checked against @param/@return ==\n";
show("kind(1)", Doc::observe('kind', 1));
show("kind('a')", Doc::observe('kind', 'a'));
show("kindBuggy(1)", Doc::observe('kindBuggy', 1));
show("id(new ArrayObject)", Doc::observe('id', new ArrayObject()));
show("idBuggy(new ArrayObject)", Doc::observe('idBuggy', new ArrayObject()));

echo "\n== 3. property: @param as generator, @return/@throws as the property ==\n";
foreach (['kind', 'kindBuggy', 'maxOf', 'label', 'invert'] as $f) echo Doc::property($f, seed: 7), "\n";
