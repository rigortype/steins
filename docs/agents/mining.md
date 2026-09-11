# Re-mining the generated tables

Four of the catalog's tables are generated from engines rather than written by
hand, and all four take the same repeatable `--php` flag. The engines are a real
input: a row mined from one build is a claim about every build a target may run
on, so what a run is given decides what the table is worth.

| table | command | what the extra engines buy |
| --- | --- | --- |
| `phpsrc-mining/constants.toml` | `mine-constants` | `since`/`until` from presence, and a value the engines disagree about is refused (ADR-0094 §2) |
| `phpstan-mining/declared_returns.toml` | `mine-function-map --functions` | a lower minor vetoes a row it contradicts (ADR-0069 §3) |
| `phpstan-mining/declared_method_returns.toml` | `mine-function-map --methods` | the same |
| `phpsrc-mining/param_facts.toml` | `mine-param-facts` | a platform's own builtins, and a by-ref disagreement refused rather than merged (issue #703) |

Run `cargo xtask gen-catalog` after any of them, and commit the TOML and the
generated `.rs` together — `gen-catalog --check` is a CI gate and the two are one
artefact.

## The engine set

One engine per PHP minor, low first, the top one last. On the maintainer's Mac
they are the nix `php-with-extensions` derivations plus the Homebrew build:

```sh
ls -d /nix/store/*php-with-extensions*/bin/php   # 8.2, 8.3, 8.4
which php                                        # Homebrew, 8.5
```

`mine-constants` refuses two engines of the same minor outright: presence is read
one engine per minor, and two answers for one column is not a table.

### The engines at a boundary have to be comparable

The set above is **two packagers**, and that is a real cost. php-src registers
whole blocks of constants behind the linked library's version or a configure-time
feature, so the nix→Homebrew step between 8.4 and 8.5 carries differences that
have nothing to do with the minor: libxml2 2.15.3 against 2.9.13, `--with-mhash`
on one side only, `HAVE_LDAP_SASL` on one side only, zlib compiled in against
loaded as a shared module.

`mine-constants` therefore records a **build fingerprint** per engine
(`[engines.build]` in the TOML) and mints a minor boundary only when the two
engines forming it agree on every fact that extension's registrations are guarded
on; a departure (`until`) additionally needs the same packager on both sides.
What this takes back is listed by name in `[declined.build_parity]` with the fact
that differed — read it before concluding a constant arrived in a minor.

The fingerprints are a mitigation and not a fix. **One packager for all four
minors is the right answer** the day a PHP 8.5 build from the same source as the
other three is reachable (`nix eval nixpkgs#php85.version`); re-mine then, and the
declined boundaries above come back as real answers.

## The Linux half of `param_facts.toml`

`param_facts.toml` is the one table whose universe is a **platform's** and not
only a version's. `chroot` is a Linux builtin, no Darwin PHP has it at any minor,
and nix cannot build a Linux PHP on an Apple-silicon Mac without a Linux builder
this machine does not have. So the Linux engine comes from CI, and its rows are
merged deliberately:

1. Dispatch the job — it runs only when the dispatch asks for it by name, so
   neither a push nor a webhook-escape-hatch re-run pays its twenty minutes:

   ```sh
   gh workflow run ci.yml --ref <branch> -f mine_param_facts=true
   ```

2. Download the artefact once the run finishes:

   ```sh
   gh run download <run-id> -n param-facts-linux -D /tmp/pf
   ```

3. Merge it as one more source, with the local engines, and regenerate:

   ```sh
   cargo xtask mine-param-facts --php <8.2> --php <8.3> --php <8.4> --php <8.5> \
     --merge /tmp/pf/param_facts.toml
   cargo xtask gen-catalog
   ```

   `--merge` reads a previously-mined `param_facts.toml` back in its own shape —
   its rows, the `platforms` each already records, and its `[refused.by_ref]`
   findings — so the CI job runs the same command a maintainer does and nothing
   needs converting on the way down.

   **`--merge` sources are always appended last**, whatever order the flags are
   written in, so the merged table's signature spellings win: the last source that
   has a name supplies `params`, `param_names`, `optional` and `params_required`
   whole. `[meta] extensions` is NOT last-wins — it is the union over every
   source, because `mine-constants` reads that list back as its own allowlist and
   a CI build with a narrower extension set would otherwise delete every `ast\*`,
   `BROTLI_*` and `GNUPG_*` constant from the other table on the next run.

4. Read the run's output. `[refused.by_ref]` names every function the sources
   disagree about, with the positions each reported — those rows are left out of
   the table entirely, and a new entry is a finding, not noise.

Until step 3 has been done on a branch, that branch's committed table is
**macOS-only**, whatever `[meta] engines` says about minors: check the `os`
column there before claiming a platform is covered.
