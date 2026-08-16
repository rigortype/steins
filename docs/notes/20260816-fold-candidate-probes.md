# Fold candidates, measured — the first `fold-probe` batch (2026-08-16)

> **Acted on (2026-08-16).** The three divergent names were admitted as
> `Refused` rows in ADR-0066's wave-3 amendment — they fold on the project's own
> 64-bit PHP and decline in the browser. The thirteen clean names are **not**
> thereby admitted: a width verdict is one of the two things a row needs, and
> the paragraph below is why.

**This is evidence, not a decision.** A width verdict is one of the two things an
allowlist row needs; the other is the ADR-0008 purity/determinism argument, which
probing does not supply and which is not attempted here. A name measured clean
below is *not thereby admissible* — it is a name whose width question is settled
and whose remaining question is the other one.

## What was run

`cargo xtask fold-probe --names …`, in **both calling conventions**, against
64-bit `php` 8.5.9 and 32-bit php-wasm 0.1.0 (PHP 8.5.2, `PHP_INT_SIZE = 4`)
through the same `steins_handle` dispatch core, comparing response **bytes**.

The sixteen names are the top of the coverage survey's *"candidate (nothing
answers)"* section (`20260815-phpstan-type-php-coverage.md`) after removing six
that no width probe can settle:

| removed | why |
| --- | --- |
| `ob_get_clean`, `ob_get_contents` | output-buffer effects; folding is out of the question before width is asked |
| `hash_file` | reads the filesystem |
| `compact` | reads the symbol table (the ADR-0046 world) |
| `class_implements` | object world |
| `filter_var` | its behaviour is a function of the filter constant and its options array; a width probe on the shape it actually gets is a slice of its own |

## The disposition

Counts are `probes (silent/reverse/decline)`, identical in both conventions
except where noted.

| name | corpus | weak | strict | width verdict |
| --- | ---: | --- | --- | --- |
| `json_encode` | 133 | 42 (2/0/4) | 42 (2/0/0) | **diverges** |
| `json_decode` | 116 | 46 (3/0/0) | 46 (3/0/0) | **diverges** |
| `preg_match_all` | 14 | 72 (2/0/4) | 72 (2/0/0) | **diverges** (the PCRE build option) |
| `max` | 76 | 30 (0/0/0) | 30 (0/0/0) | clean |
| `min` | 40 | 30 (0/0/0) | 30 (0/0/0) | clean |
| `parse_url` | 48 | 25 (0/0/0) | 25 (0/0/0) | clean |
| `array_search` | 36 | 19 (0/0/1) | 19 (0/0/1) | clean |
| `array_diff` | 16 | 20 (0/0/4) | 20 (0/0/4) | clean |
| `array_intersect` | 14 | 20 (0/0/4) | 20 (0/0/4) | clean |
| `array_intersect_key` | 13 | 20 (0/0/4) | 20 (0/0/4) | clean |
| `array_column` | 11 | 52 (0/0/1) | 52 (0/0/1) | clean |
| `array_pad` | 11 | 28 (0/0/1) | 28 (0/0/1) | clean |
| `array_diff_key` | — | 20 (0/0/4) | 20 (0/0/4) | clean |
| `array_sum` | — | 8 (0/0/1) | 8 (0/0/1) | clean |
| `array_chunk` | — | 17 (0/0/0) | 17 (0/0/0) | clean |
| `pathinfo` | — | 25 (0/0/2) | 25 (0/0/0) | clean |

**Zero reverse verdicts** anywhere: no case where the narrow engine answers and
the wide one declines.

## The three divergences

### `json_decode` — the machine types the number it parses

| probe | 64-bit | 32-bit |
| --- | --- | --- |
| `json_decode("3000000000")` | `int(3000000000)` | `float(3000000000.0)` |
| `json_decode("2147483648")` | `int(2147483648)` | `float(2147483648.0)` |

The document is the same text; the value's **type tag** is the parsing engine's
word size. This is `range`'s shape exactly — a numeric run typed by the machine
rather than by the argument — and it needs no flags, no options, and no unusual
input. Any JSON containing an integer past 2³¹ diverges.

### `json_encode` — two flags that only diverge together

```text
json_encode("3000000000", JSON_NUMERIC_CHECK|JSON_PRESERVE_ZERO_FRACTION)
  64-bit: "3000000000"     32-bit: "3000000000.0"
```

`JSON_NUMERIC_CHECK` retypes the numeric string; the narrow engine has no int
that wide, so it becomes a float; `JSON_PRESERVE_ZERO_FRACTION` then renders the
fraction. **Neither flag alone diverges** — this is the pairwise hazard pass
finding a case one-at-a-time generation cannot reach, and worth remembering when
reading any single-argument probe as "clean".

### `preg_match_all` — the inline limit verbs, again

| probe | 64-bit (PCRE 10.47, JIT) | 32-bit (PCRE 10.44, no JIT) |
| --- | --- | --- |
| `preg_match_all('/(*LIMIT_MATCH=1)a/', "aaa")` | `3` | `false` |
| `preg_match_all('/(*LIMIT_RECURSION=1)(?:a)+/', "aaa")` | `1` | `false` |

The third name on the axis `preg_split` opened and `preg_match` joined. It is not
about the word size at all, and a `Refused` row for it would carry
`RefusalAxis::BuildOption` like its two siblings.

## What a reader of this note still owes each name

- **the ADR-0008 argument** — purity and determinism, which no probe measures;
- **for `array_diff`/`array_intersect` and their `_key` relatives**: the `u*`
  variants of the same families take a comparator at a variadic `mixed` tail and
  are a different question entirely; nothing here says anything about them;
- **for `parse_url`/`pathinfo`**: both return shapes whose keys depend on the
  input, so the value-domain question (can the result be carried?) is separate
  from the width question answered here;
- **for `max`/`min`**: PHP's comparison rules across mixed types are the
  interesting part, and the probes above exercise the families the generator
  builds, not a survey of comparison edge cases.
