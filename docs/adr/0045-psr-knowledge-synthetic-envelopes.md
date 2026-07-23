# PSR knowledge: vendor recovers the types, the engine supplies the semantics

Survey of php-fig/psr (12 interface packages, 45 src files, 2026-07-23):
zero `@template`/`@psalm-*`/`@phpstan-*` annotations exist upstream, and
the checkouts are byte-identical to what projects vendor — so the
interface inventory, hierarchy, and native signatures come free from
parsing `vendor/psr/*`. Engine-supplied knowledge is only what the
declared types cannot say:

1. **A fourth ADR-0044 computation kind — passthrough**: return = a
   proven input. PSR-14 `dispatch(object): object` is spec-guaranteed to
   return *the same object* (MUST) — the value flows through, no
   template solver (ADR-0032). PSR-7/13 `with*()` phpdoc `@return
   static` over a widened native parent type is receiver-passthrough
   (return = the receiver's proven class). Version skew is real: PSR-7
   v1 has no native return types at all — the vendored signature is
   read, never assumed (ask-the-real-thing).
2. **PSR-11 `get(T::class) → T` is NOT adopted by default.** The spec
   explicitly disclaims identifier semantics ("callers SHOULD NOT
   assume the structure of the string carries any semantic meaning") —
   the class-string heuristic is a framework convention, so it ships as
   an opt-in pack setting, and where the concrete container is
   in-project, propagation can *verify* the convention instead of
   assuming it (the ADR-0037 ladder: assumption < verified).
3. **Synthetic throws envelopes** carry the spec-only WHEN rules the
   phpdoc lacks: PSR-18's three-way Client/Request/Network dispatch,
   PSR-11 `!has ⇒ get throws NotFound`, PSR-3 invalid-level, PSR-6/16
   interface `InvalidArgumentException` on key methods. Expressible in
   the ADR-0039 v1 wire format (phpdoc strings), packaged with the PSR
   pack.
4. **Value-domain rules reserved for future lints** (not v1): PSR-16
   key charset (`{}()/\@:` forbidden, ≤64 chars), PSR-3's 8-level set +
   `{placeholder}`→context-key convention. Provable against Singleton/
   OneOf facts when a boundary profile wants them.
5. **Out of scope**: psr/link (niche; its `@return static` pattern
   rides the generic passthrough anyway), per-attributes (a Placeholder
   class today — watch item), hierarchy/catalog duplication of anything
   vendor parsing already yields.

Packaged as the first mostly-declarative ADR-0044 pack (synthetic
envelopes + passthrough shapes); sequenced with the data-mapper packs
after ADR-0043's method stages.
