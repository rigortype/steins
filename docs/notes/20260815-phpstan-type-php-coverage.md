# PHPStan `src/Type/Php` coverage against the Steins fold allowlist

Generated 2026-08-15 from `phpstan-src` at `8dc0c8fdc` and Steins at master
`0afca4a` (the issue #354 merge). This is a **measurement, not a decision**: it
says what Steins answers today for each builtin PHPStan writes a dedicated
extension for, and sorts them by what a fold would have to earn.

## Method

PHPStan's `src/Type/Php/` holds 190 files. The function names each extension
declares support for were taken from its `isFunctionSupported()` body plus every
class-level array literal, keeping the quoted tokens that are resident internal
functions — **238 distinct builtins** across 140 files. (An unscoped scan finds
243; the five extra are array *keys* in `pathinfo`'s result shape, a feature
check, and an internal call, not declared support.)

Each name was then given the **minimum-arity all-literal call** its own arginfo
admits (`string` → `"ab"`, `int` → `2`, `array` → `["a", "b"]`, and so on) and
that call was dumped through `steins check` twice, with and without the PHP
sidecar. The call is not meant to be useful; it is the shape the fold seam
requires, so what Steins answers for it is what Steins can answer at all.

**One measurement trap, recorded because it silently zeroed a whole run.** All
215 calls initially lived in one file as top-level statements, and a single
`compact("ab")` at line 59 widened *every* dump in the file, including the 58
before it — the ADR-0046 dam is scope-wide. Each call now sits in its own
wrapper function. A file-level sequence of `dumpType` calls is not a set of
independent probes.

## Disposition

| bucket | names | what it means |
| --- | ---: | --- |
| candidate (nothing answers) | 76 | no rung, no refinement — an all-literal call gets the declared floor or nothing |
| candidate (a type answers) | 31 | something answers a *type*; a fold would upgrade it to a value (ADR-0028 §5 admits this) |
| rung answers a VALUE | 12 | a Rust rule already computes the value **and covers non-literal arguments** — §5 excludes these |
| on the allowlist | 47 | already folds (39 safe / 6 refused / 2 unverified) |
| environment oracle | 14 | `function_exists`-shaped: answered by `reflect` (ADR-0049), not by a value |
| excluded (effect label) | 8 | carries a `nondet.*`/`io.*` label — can never fold |
| excluded (documented) | 27 | excluded in prose (ADR-0008 / the catalog module doc / the ADR-0066 amendments) |
| fold inexpressible | 23 | a required parameter no literal can fill: callable, by-ref, object/resource |
| **total** | **238** | |

The 107 candidates carry **2437 of the 13346 builtin calls** the FP-gate
corpus makes among these names.

## Two things this surfaced on the way

1. **Four aliases of allowlisted names are not themselves allowlisted.**
   *(Acted on: the four landed as `WIDTH_SAFE` rows the same day — see the
   ADR-0066 amendment of 2026-08-15. The paragraph below is the finding as it
   stood when this note was measured.)*
   `join`/`implode`, `chop`/`rtrim`, `sizeof`/`count`, `doubleval`/`floatval` —
   each pair is one C handler under two spellings, verified here by identical
   arginfo (same parameter names, types, optionality and return type). Folding
   the alias is exactly as safe as folding the name it aliases, and there is no
   probe to run: the alias cannot diverge from its target on any machine,
   because there is only one implementation. `foldable()` matches the spelling,
   so all four widen today. A fifth pair, `key_exists`/`array_key_exists`, has
   the same relationship but its target is not admitted either, so the two ride
   together whenever `array_key_exists` is taken up. The same holds for the
   `is_integer`/`is_long`/`is_int` and `is_double`/`is_float` families.
2. **`strtotime` and `idate` carry no effect label.** The catalog's module doc
   states both are `nondet.time` and timezone-coupled, and the ADR-0066
   amendment records the probes that established it — but `effect_labels()`
   answers `None` (uncatalogued) for both, where `date` answers
   `["nondet.time"]`. Uncatalogued widens, so nothing is unsound; the exclusion
   is simply not readable as data, which is exactly what a mechanical screen for
   a larger expansion would consult.

## candidate (nothing answers) — 76

Ordered by corpus frequency. The third column is what an all-literal call dumps
today with the sidecar live.

| name | corpus | dumps today |
| --- | ---: | --- |
| `strpos` | 303 | int<0, max>\|false (asserted) |
| `preg_match` | 220 | 0\|1\|false (asserted) |
| `array_filter` | 176 | array (asserted) |
| `json_encode` | 133 | non-empty-string\|false (asserted) |
| `json_decode` | 116 | unknown |
| `max` | 76 | unknown |
| `parse_url` | 48 | int\|string\|false\|null\|array (asserted) |
| `stripos` | 41 | int<0, max>\|false (asserted) |
| `min` | 40 | unknown |
| `array_search` | 36 | int\|string\|false (asserted) |
| `filter_var` | 26 | unknown |
| `strrpos` | 26 | int\|false (asserted) |
| `array_diff` | 16 | array (asserted) |
| `array_intersect` | 14 | array (asserted) |
| `preg_match_all` | 14 | int<0, max>\|false (asserted) |
| `array_intersect_key` | 13 | array (asserted) |
| `array_column` | 11 | array (asserted) |
| `array_pad` | 11 | array (asserted) |
| `hash_file` | 9 | non-falsy-string\|false (asserted) |
| `pathinfo` | 9 | string\|array (asserted) |
| `ob_get_clean` | 8 | string\|false (asserted) |
| `array_diff_key` | 6 | array (asserted) |
| `array_sum` | 6 | int\|float (asserted) |
| `class_implements` | 6 | false\|array<string, class-string> (asserted) |
| `array_chunk` | 5 | list<array> (asserted) |
| `compact` | 5 | array<string, mixed> (asserted) |
| `ob_get_contents` | 5 | string\|false (asserted) |
| `array_change_key_case` |  | array (asserted) |
| `array_count_values` |  | array<int<1, max>> (asserted) |
| `array_diff_assoc` |  | array (asserted) |
| `array_diff_uassoc` |  | array (asserted) |
| `array_diff_ukey` |  | array (asserted) |
| `array_intersect_assoc` |  | array (asserted) |
| `array_intersect_uassoc` |  | array (asserted) |
| `array_intersect_ukey` |  | array (asserted) |
| `array_rand` |  | unknown |
| `array_replace` |  | array (asserted) |
| `array_udiff` |  | array (asserted) |
| `array_udiff_assoc` |  | array (asserted) |
| `array_udiff_uassoc` |  | array (asserted) |
| `array_uintersect` |  | array (asserted) |
| `array_uintersect_assoc` |  | array (asserted) |
| `array_uintersect_uassoc` |  | array (asserted) |
| `class_parents` |  | false\|array<string, class-string> (asserted) |
| `class_uses` |  | false\|array<string, class-string> (asserted) |
| `count_chars` |  | string\|array<int, int> (asserted) |
| `date_create` |  | unknown |
| `date_create_from_format` |  | unknown |
| `date_create_immutable` |  | unknown |
| `date_create_immutable_from_format` |  | unknown |
| `filter_input` |  | unknown |
| `filter_input_array` |  | false\|null\|array (asserted) |
| `filter_var_array` |  | false\|null\|array (asserted) |
| `fscanf` |  | unknown |
| `fstat` |  | false\|array (asserted) |
| `get_defined_vars` |  | array<string, mixed> (asserted) |
| `get_parent_class` |  | class-string\|false (asserted) |
| `hash_hmac_file` |  | non-falsy-string\|false (asserted) |
| `highlight_string` |  | string\|bool (asserted) |
| `localtime` |  | array (asserted) |
| `lstat` |  | false\|array (asserted) |
| `ob_get_flush` |  | string\|false (asserted) |
| `ob_get_length` |  | int\|false (asserted) |
| `openssl_cipher_iv_length` |  | int\|false (asserted) |
| `openssl_cipher_key_length` |  | unknown |
| `openssl_encrypt` |  | string\|false (asserted) |
| `pow` |  | unknown |
| `preg_filter` |  | string\|null\|array (asserted) |
| `preg_replace_callback_array` |  | string\|null\|array (asserted) |
| `sscanf` |  | unknown |
| `stat` |  | false\|array (asserted) |
| `str_ireplace` |  | string\|array<string> (asserted) |
| `str_word_count` |  | unknown |
| `strripos` |  | int\|false (asserted) |
| `strstr` |  | string\|false (asserted) |
| `strtok` |  | non-empty-string\|false (asserted) |

## candidate (a type answers) — 31

| name | corpus | what a fold would replace |
| --- | ---: | --- |
| `is_array` | 418 | fold would upgrade bool to a value |
| `array_key_exists` | 210 | fold would upgrade bool to a value |
| `preg_replace` | 119 | fold would upgrade array\|null to a value |
| `get_debug_type` | 70 | fold would upgrade non-falsy-string to a value |
| `round` | 59 | fold would upgrade float to a value |
| `floor` | 43 | fold would upgrade float to a value |
| `hash` | 40 | fold would upgrade string to a value |
| `ceil` | 18 | fold would upgrade float to a value |
| `escapeshellarg` | 17 | fold would upgrade non-falsy-string to a value |
| `addcslashes` | 13 | fold would upgrade non-falsy-string to a value |
| `define` | 13 | fold would upgrade bool to a value |
| `escapeshellcmd` | 12 | fold would upgrade string to a value |
| `is_iterable` | 11 | fold would upgrade bool to a value |
| `htmlspecialchars` | 9 | fold would upgrade non-falsy-string to a value |
| `ctype_digit` | 6 | fold would upgrade bool to a value |
| `bcdiv` |  | fold would upgrade string to a value |
| `bcmod` |  | fold would upgrade string to a value |
| `bcpowmod` |  | fold would upgrade string to a value |
| `bcsqrt` |  | fold would upgrade string to a value |
| `chop` |  | fold would upgrade lowercase-string to a value |
| `crc32` |  | fold would upgrade int to a value |
| `doubleval` |  | fold would upgrade float to a value |
| `fnmatch` |  | fold would upgrade bool to a value |
| `get_called_class` |  | fold would upgrade string to a value |
| `hash_hkdf` |  | fold would upgrade string to a value |
| `hash_hmac` |  | fold would upgrade string to a value |
| `hash_pbkdf2` |  | fold would upgrade string to a value |
| `htmlentities` |  | fold would upgrade non-falsy-string to a value |
| `join` |  | fold would upgrade lowercase-string to a value |
| `key_exists` |  | fold would upgrade bool to a value |
| `vsprintf` |  | fold would upgrade string to a value |

## rung answers a VALUE — 12

ADR-0028 §5 excludes these: the Rust rung already computes the value **and**
covers arguments a fold never can. Admitting them would buy a second
implementation of the same answer plus a fixture to keep the two agreeing.

| name | corpus | the rung's answer |
| --- | ---: | --- |
| `array_keys` | 187 | §5: a fold would duplicate it — list{0, 1} |
| `array_values` | 122 | §5: a fold would duplicate it — list{'a', 'b'} |
| `array_slice` | 41 | §5: a fold would duplicate it — array{} |
| `array_reverse` | 32 | §5: a fold would duplicate it — list{'b', 'a'} |
| `array_flip` | 22 | §5: a fold would duplicate it — array{a: 0, b: 1} |
| `array_combine` | 15 | §5: a fold would duplicate it — array{a: 'a', b: 'b'} |
| `array_first` | 8 | §5: a fold would duplicate it — 'a' |
| `array_fill_keys` | 6 | §5: a fold would duplicate it — array{a: 'ab', b: 'ab'} |
| `array_key_first` | 5 | §5: a fold would duplicate it — 0 |
| `array_key_last` |  | §5: a fold would duplicate it — 1 |
| `array_last` |  | §5: a fold would duplicate it — 'b' |
| `sizeof` |  | §5: a fold would duplicate it — 2 |

## environment oracle — 14

| name | corpus | why not a fold |
| --- | ---: | --- |
| `function_exists` | 442 | answered by reflect (ADR-0049), not by a value |
| `defined` | 319 | answered by reflect (ADR-0049), not by a value |
| `constant` | 160 | answered by reflect (ADR-0049), not by a value |
| `class_exists` | 157 | answered by reflect (ADR-0049), not by a value |
| `iterator_to_array` | 118 | answered by reflect (ADR-0049), not by a value |
| `get_class` | 104 | answered by reflect (ADR-0049), not by a value |
| `method_exists` | 97 | answered by reflect (ADR-0049), not by a value |
| `is_callable` | 67 | answered by reflect (ADR-0049), not by a value |
| `is_a` | 22 | answered by reflect (ADR-0049), not by a value |
| `is_subclass_of` | 20 | answered by reflect (ADR-0049), not by a value |
| `interface_exists` | 17 | answered by reflect (ADR-0049), not by a value |
| `property_exists` | 6 | answered by reflect (ADR-0049), not by a value |
| `enum_exists` |  | answered by reflect (ADR-0049), not by a value |
| `trait_exists` |  | answered by reflect (ADR-0049), not by a value |

## excluded (effect label) — 8

| name | corpus | label |
| --- | ---: | --- |
| `microtime` | 132 | nondet.time |
| `date` | 63 | nondet.time |
| `ini_get` | 63 | global.read |
| `mt_rand` | 57 | nondet.random |
| `rand` | 29 | nondet.random |
| `hrtime` | 17 | nondet.time |
| `mb_regex_encoding` |  | global.write |
| `random_int` |  | nondet.random |

## excluded (documented) — 27

| name | corpus | reason on record |
| --- | ---: | --- |
| `assert` | 627 | zend.assertions decides whether it runs at all |
| `trigger_error` | 191 | raises a diagnostic — an effect, uncoloured |
| `mb_strtolower` | 25 | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_substr` | 22 | mbstring.internal_encoding; php-wasm has no mbstring |
| `strtotime` | 20 | nondet.time + timezone-coupled (module doc; NOT carried as a label) |
| `number_format` | 16 | held out conservatively (ADR-0066 amendment) |
| `mb_strlen` | 15 | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_convert_encoding` | 6 | mbstring.internal_encoding; php-wasm has no mbstring |
| `gettimeofday` |  | nondet.time |
| `idate` |  | timezone-coupled even with an explicit timestamp (module doc; NOT a label) |
| `mb_chr` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_convert_case` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_convert_kana` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_encoding_aliases` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_http_output` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_internal_encoding` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_lcfirst` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_ord` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_str_split` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_stripos` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_strpos` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_strripos` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_strrpos` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_strstr` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_strtoupper` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_substitute_character` |  | mbstring.internal_encoding; php-wasm has no mbstring |
| `mb_ucfirst` |  | mbstring.internal_encoding; php-wasm has no mbstring |

## fold inexpressible — 23

| name | corpus | blocking parameter |
| --- | ---: | --- |
| `array_map` | 245 | callable parameter |
| `array_shift` | 90 | by-ref parameter |
| `array_pop` | 48 | by-ref parameter |
| `preg_replace_callback` | 35 | callable parameter |
| `reset` | 34 | object/resource parameter |
| `array_splice` | 31 | by-ref parameter |
| `end` | 30 | object/resource parameter |
| `array_any` | 18 | callable parameter |
| `key` | 17 | object/resource parameter |
| `current` | 13 | object/resource parameter |
| `array_reduce` | 8 | callable parameter |
| `curl_getinfo` | 5 | no literal fills a required parameter |
| `array_all` | 4 | callable parameter |
| `array_find` |  | callable parameter |
| `array_find_key` |  | callable parameter |
| `array_walk` |  | callable parameter |
| `date_format` |  | no literal fills a required parameter |
| `date_interval_format` |  | no literal fills a required parameter |
| `mb_parse_str` |  | by-ref parameter |
| `next` |  | object/resource parameter |
| `parse_str` |  | by-ref parameter |
| `prev` |  | object/resource parameter |
| `settype` |  | by-ref parameter |

## on the allowlist — 47

| name | corpus | width class |
| --- | ---: | --- |
| `sprintf` | 1453 | refused |
| `count` | 897 | safe |
| `implode` | 594 | safe |
| `in_array` | 441 | safe |
| `trim` | 432 | safe |
| `substr` | 404 | safe |
| `array_merge` | 356 | unverified |
| `strlen` | 346 | safe |
| `str_replace` | 288 | safe |
| `explode` | 256 | unverified |
| `strtolower` | 209 | safe |
| `str_repeat` | 181 | safe |
| `str_starts_with` | 163 | safe |
| `rtrim` | 148 | safe |
| `str_contains` | 119 | safe |
| `strtr` | 117 | safe |
| `preg_quote` | 85 | safe |
| `ucfirst` | 85 | safe |
| `abs` | 64 | refused |
| `version_compare` | 59 | refused |
| `ltrim` | 57 | safe |
| `str_ends_with` | 43 | safe |
| `array_unique` | 37 | safe |
| `str_pad` | 31 | safe |
| `strtoupper` | 30 | safe |
| `gettype` | 29 | safe |
| `rawurlencode` | 28 | safe |
| `range` | 24 | refused |
| `preg_split` | 20 | refused |
| `base64_decode` | 17 | safe |
| `str_split` | 14 | safe |
| `intdiv` | 11 | safe |
| `urlencode` | 10 | safe |
| `substr_replace` | 9 | safe |
| `array_fill` | 8 | safe |
| `intval` | 8 | refused |
| `rawurldecode` | 8 | safe |
| `addslashes` |  | safe |
| `boolval` |  | safe |
| `floatval` |  | safe |
| `lcfirst` |  | safe |
| `str_decrement` |  | safe |
| `str_increment` |  | safe |
| `strrev` |  | safe |
| `strval` |  | safe |
| `ucwords` |  | safe |
| `urldecode` |  | safe |
