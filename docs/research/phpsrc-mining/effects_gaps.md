# P4 — Effect-label taxonomy audit (vocabulary gaps vs Steins `known_labels`)

php-src commit: `6bc7c26cf67a9480b5ef9d6191aebe87fa931183` (Thu Jul 9 2026)
Runtime cross-checks vs PHP 8.5.8 (cli).

This is a **vocabulary audit** (ADR-0018 registry), not a per-function effect
table. For each side-effect KIND actually present in php-src's standard-library
surface I state whether Steins' registry already has a node for it, and for each
gap I propose a dot-path label in the registry's prefix-subsumption style.

> **Update 2026-08-16 (issue #382 follow-up).** The two *seeding* gaps this
> audit recorded are closed. §5's handler-registration set is coloured
> `global.write` (with `spl_autoload_unregister`, the `stream_wrapper_*` trio
> and the tick-function pair beside the names proposed here), and the process
> family is whole: `exec`, `shell_exec`, `popen` and `proc_open` carry
> `io.process`, kept apart from `system`/`passthru`'s `io.process` + `io.output`
> because those two RELAY the child's output and these four hand it back. The
> procedural database family is coloured too, by one rule — **talks to the
> server**: connecting and closing, sending a statement (including the async
> `pg_send_*`/`mysqli_reap_async_query` spellings and the `COPY` pair), and the
> transaction control that sends `COMMIT`/`ROLLBACK`, across `mysqli_*`, `pg_*`
> and `odbc_*`. What stays uncoloured is the buffered/local half — error and
> metadata accessors, `mysqli_real_escape_string`, and the `*_fetch_*` families
> — because on an UNBUFFERED result some of those do reach the wire, and telling
> those apart is a property of the call site's earlier `MYSQLI_USE_RESULT` that
> a name-keyed table cannot see. That is the same shape as `fwrite`'s `STDOUT`
> destination below, deferred for the same reason.
>
> **Every gap this audit recorded, vocabulary and seeding alike, is now closed
> or explicitly deferred with its reason.**
>
> **Update 2026-08-12 (ADR-0083).** Every gap ranked below has since landed,
> and the `output` root the audit was written against no longer exists: output
> is now an ambient channel under `io` (`io.output`, split by
> `ob_start()`-capturability), with `io.input` beside it. Read `output` as
> `io.output` and `output.header` as `io.output.header` throughout the
> proposal sections; the registry block and the coverage table below are
> current as of that ADR.

## Steins' current registry (`known_labels()`)

```
exit   ffi
global.read   global.write
io   io.db   io.fs   io.fs.read   io.fs.write   io.input   io.ipc
     io.net   io.net.http   io.process   io.signal
     io.output   io.output.buffer   io.output.header
                 io.output.stderr   io.output.stdout
mutate   mutate.local
nondet   nondet.random   nondet.time
failure   failure.environment   failure.input   failure.resource
```

## Effect kinds present in the stdlib, mapped to the registry

| Kind (php-src surface) | Example functions | Registry node | Verdict |
|---|---|---|---|
| Filesystem read | `fread`, `file_get_contents`, `scandir`, `stat` | `io.fs.read` | covered |
| Filesystem write | `fwrite`, `file_put_contents`, `mkdir`, `unlink` | `io.fs.write` | covered |
| Generic network | `fsockopen`, `stream_socket_client`, `curl_exec` | `io.net` | covered |
| HTTP client | `curl_exec` (http), `file_get_contents("http://…")` | `io.net.http` | covered |
| Database | `mysqli_query`, `PDO::query`, `pg_query` | `io.db` | covered (node exists; no builtin colored yet) |
| Process spawn | `exec`, `proc_open`, `popen`, `shell_exec` | `io.process` | node exists; only the relaying pair below is colored |
| Process spawn **that relays output** | `system`, `passthru` | `io.process` + `io.output` | covered (ADR-0083) |
| Stdout output (language) | `echo`/`print`/`<?=`, inline HTML | `io.output.buffer` | covered (origins, not catalog rows) |
| Stdout output (functions) | `printf`, `vprintf`, `var_dump`, `print_r`, `var_export`, `flush`, `ob_flush` | `io.output.buffer` | covered |
| Read-and-relay to output | `readfile`, `fpassthru` | `io.fs.read` + `io.output.buffer` | covered (ADR-0083; uncolored before it) |
| Environment mutate | `putenv`, `setlocale`, `ini_set` | `global.write` | covered |
| Environment/ini read | `getenv`, `ini_get` | `global.read` | covered |
| Randomness | `rand`, `random_int`, `random_bytes` | `nondet.random` | covered |
| Wall-clock time | `time`, `microtime`, `date` | `nondet.time` | covered |
| Termination | `exit`, `die` | `exit` | covered (language construct) |
| **Signals** | `pcntl_signal`, `pcntl_alarm`, `pcntl_async_signals`, `pcntl_sigprocmask`, `posix_kill` | — | **GAP** |
| **HTTP response headers** | `header`, `header_remove`, `setcookie`, `setrawcookie`, `http_response_code` | — | **GAP** |
| **IPC / shared memory** | `shmop_write`, `sem_acquire`, `msg_send`, `sysvshm`, `apcu_*` (ecosystem) | — | **GAP (partial)** |
| **FFI (opaque native)** | `FFI::cdef`, `FFI::new`, any `FFI\CData` call | — | **GAP** |
| **Global handler / dispatch registration** | `set_error_handler`, `set_exception_handler`, `spl_autoload_register`, `stream_wrapper_register`, `ob_start` | `global.write`? | **borderline** |
| **RNG state seeding** | `srand`, `mt_srand`, `random_*` engine seeding | `global.write`? | **borderline** |
| Output-buffer state | `ob_start`, `ob_get_clean`, `ob_end_flush` | — | **uncatalogued on purpose**: unknown-effect widening is the sound default until effect masking exists (ADR-0083 deferral) |
| Ambient input stream | `php://input`, `php://stdin` | `io.input` | node exists; no row, since narrowing a stream by its target needs argument awareness |
| Session state | `session_start`, `session_write_close`, `session_regenerate_id` | composite | note |

---

## Gaps that matter for effect-envelope (`@`-envelope) checking

### 1. `io.signal` — signal delivery/handling  **(recommended)**
- **Functions:** `pcntl_signal`, `pcntl_signal_dispatch`, `pcntl_alarm`,
  `pcntl_async_signals`, `pcntl_sigprocmask`, `pcntl_sigwaitinfo`, `posix_kill`.
- **Why it matters:** registering a signal handler / sending a signal is a real
  observable OS interaction that a `Pure` or even `io.fs`-scoped envelope must
  not silently admit. Today these are uncatalogued (→ `None` → widen), so an
  envelope violation would be missed. A daemon/worker codebase (the kind Steins
  targets) uses these heavily and would legitimately declare `@effects io.signal`.
- **Placement:** child of `io` (`io.signal`), parallel to `io.process`. Prefix
  subsumption then lets a coarse `@effects io` admit it.

### 2. `output.header` — HTTP response header mutation  **(recommended)**
- **Functions:** `header`, `header_remove`, `setcookie`, `setrawcookie`,
  `http_response_code`, and indirectly `session_start` (sends `Set-Cookie`).
- **Why it matters:** sending headers is distinct from writing stdout, but is a
  sibling response-side effect. Framework/domain-layer policies frequently want
  "no header mutation from the domain layer" — the semantic-label use case
  ADR-0018 calls out. Folding it under bare `output` is defensible (both are
  response effects) but loses the ability to target headers specifically.
- **Placement:** child of `output` (`output.header`). A declared `output`
  subsumes it; a policy can name `output.header` precisely.
- **Note:** header() also *reads* whether output already started (can fail) but
  the effect is the mutation.

### 3. `ffi` — opaque native boundary  **(recommended, high-severity)**
- **Functions:** `FFI::cdef`, `FFI::load`, `FFI::new`, `FFI::cast`, any method
  call on an `FFI\CData`/`FFI\CType` object.
- **Why it matters:** an FFI call runs arbitrary C — it can do *any* effect and
  the catalog can prove nothing about it. It deserves its own top-level label so
  an envelope can neither accidentally admit it under a narrow color nor be
  forced to widen everything. This is the "unsafe boundary" marker.
- **Placement:** top-level `ffi` (not under `io` — it is broader than I/O and is
  really an escape hatch). Analogous to how `exit` and `mutate` are top-level.

### 4. `io.ipc` — System-V / shared-memory IPC  **(optional)**
- **Functions:** `shmop_read`/`shmop_write`, `sem_acquire`/`sem_release`,
  `msg_send`/`msg_receive`, `sysvshm` family.
- **Why it matters:** shared-memory and semaphore ops are cross-process shared
  state, not filesystem and not network. Currently they would only widen. Lower
  priority than 1–3 because these extensions are niche.
- **Placement:** child of `io` (`io.ipc`).

### 5. Handler/dispatch registration and RNG seeding — **fold into `global.write`**
- **Functions:** `set_error_handler`, `set_exception_handler`,
  `spl_autoload_register`, `stream_wrapper_register`, `register_shutdown_function`,
  `srand`, `mt_srand`.
- **Recommendation:** these mutate global engine dispatch / RNG state; they are
  already expressible as `global.write` (and `register_shutdown_function` is
  additionally the ADR-0033 deferred invoker). No new node needed — but they are
  **coloring gaps**: `effect_labels()` does not yet color any of them, so they
  widen today. Worth seeding `global.write` on the handler-registration set.
  - Subtlety: `mt_srand` makes subsequent `rand`/`mt_rand` deterministic — it is
    a write to the RNG state that *removes* nondeterminism. `global.write` is the
    honest coarse color; a finer `global.write.rng` is not worth it.

---

## Non-gaps worth recording

- **`io.db` and `io.process` nodes exist but are uncolored.** No builtin in
  `effect_labels()` returns them yet (mysqli/PDO/pg and exec/system/proc_open are
  all uncatalogued → widen). This is a *seeding* gap, not a *vocabulary* gap —
  the labels are ready; the catalog just hasn't colored the functions.
- **`sleep`/`usleep`** are colored `io` in Steins. That is a coarse but defensible
  choice (an observable process-timing effect). No taxonomy change needed; a
  `nondet.time`-adjacent reading is possible but `io` is fine.
- **Session** (`session_start`) is genuinely composite: `io.fs.write` (default
  file handler) + `io.output.header` (Set-Cookie) + `global.write` ($_SESSION,
  ini). It is colored with the *set* — the first client of the header label.
- **`fwrite`** stays `io.fs.write` even though its destination may be `STDOUT`.
  Narrowing to `io.output.stdout` is a syntactic check on a `STDOUT`/`STDERR`
  argument, which this name-keyed table cannot see; ADR-0083 defers it.

## Summary of proposed additions (ranked by envelope value)

1. `ffi` (top-level) — unsafe native boundary; highest severity.
2. `io.signal` — daemon/worker code needs it; currently silently widens.
3. `output.header` — response-header policies; also unblocks honest `session_*`.
4. `io.ipc` — optional; niche extensions.

All four are prefix-subsumption-clean: a coarse `@effects io` admits
`io.signal`/`io.ipc`; a coarse `@effects output` admits `output.header`; `ffi`
sits beside `exit`/`mutate` as a deliberately top-level escape hatch.
