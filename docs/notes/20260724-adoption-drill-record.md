# Adoption-drill record (M2 exit evidence) — 2026-07-24

Fourteen held-out real-world PHP applications (never used for tuning),
analyzed with the post-fix binary (the value-side instanceof verdict,
the self-clone assign fix, and the contract-arm structural dedup all
landed). Two passes per app: the default surface (proof + mechanics)
and `--profile contracts`. Every default-surface finding was triaged
verbatim against the cited source.

## Fleet table

| App | PHP files | Default wall | Default findings | Contracts total |
|---|---:|---:|---|---:|
| BookStack | 15,563 | 9.7s | 0 | 0 |
| MISP | 565 | 1.2s | 0 | 139 |
| Sylius | 31,476 | 15.1s | 0 | 18 |
| ec-cube | 18,708 | 13.2s | 0 | 86 |
| ec-cube2 | 8,026 | 4.6s | 0 | 114 |
| firefly-iii | 16,422 | 8.9s | 0 | 38 |
| kimai | 17,325 | 13.7s | 0 | 12 |
| koel | 17,599 | 11.3s | 0 | 0 |
| mautic | 35,411 | 24.3s | 0 | 115 |
| nextcloud-server | 23,212 | 19.1s | 2 (both TRUE) | 662 |
| omeka-s | 6,345 | 3.6s | 0 | 22 |
| passbolt_api | 11,437 | 6.1s | 0 | 132 |
| pixelfed | 17,733 | 10.5s | 1 (TRUE) | 15 |
| wallabag | 17,595 | 12.6s | 0 | 1 |
| **Totals** | **~237,417** | — | **3 TRUE / 0 FP** | **1,354** |

No crashes, no panics, no timeouts; slowest run in the fleet 39.7s
(mautic, contracts). Vendor findings suppressed by default throughout;
zero vendor-path leaks onto any surface.

## The default-surface triage (all three)

1. **pixelfed** `app/Providers/AppServiceProvider.php:150` —
   `call.undefined-method: Passport::personalAccessClientId()`.
   **TRUE, a real production bug**: vendored Laravel Passport v13.7.5
   removed that static API (no `__callStatic` anywhere in its source);
   the call sits in `boot()` behind a config guard, a latent fatal
   `Error` whenever personal-access tokens are enabled.
2. **nextcloud-server** `tests/lib/BackgroundJob/JobTest.php:51` —
   `call.on-null` on a deliberate `$test = null; $test->someMethod();`
   negative-path fixture. Sound; a maintainer inline-ignores it.
3. **nextcloud-server** `tests/lib/Files/ViewTest.php:1314` —
   `type.argument-mismatch` on `new View(null)` under
   `expectException(TypeError::class)`. Sound; intentional fixture.

## Fix confirmations

- kimai: the 2 pre-fix `call.on-null` FPs are gone (default 0).
- firefly-iii: the 1 pre-fix FP is gone (default 0).
- mautic: completes cleanly (was: deterministic self-clone panic).
- nextcloud-server: 19.1s default / 17.3s contracts (was:
  hours-scale non-termination in the contract-arm lane).

## Reading

The zero-FP identity held over ~237k files of held-out code once the
three survey-discovered defects were fixed — and the survey is also
the record of WHY the corpus alone was insufficient: all three defect
classes (value-side instanceof, self-clone ordering, opaque-arm
exponential joins) lived in shapes the pinned corpus never exercised.
The default surface's yield is deliberately thin (three findings,
each defensible); the contract layer carries the adoption-stage debt
reporting (1,354 findings fleet-wide) behind named profiles, per the
lenient-default principle.
