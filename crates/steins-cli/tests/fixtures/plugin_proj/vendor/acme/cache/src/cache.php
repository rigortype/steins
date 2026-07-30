<?php

declare(strict_types=1);

/**
 * The function the plugin colors `acme.cache`.
 *
 * Steins never reads this body: `steins check src/` does not analyze vendor, and
 * an ext-backed client would have no body to read anyway. The plugin's assertion
 * is the only information there is — which is exactly why it lands in the
 * declared lane and keeps its taint (ADR-0068 §1).
 */
function acme_cache_get(string $key): string
{
    return \apcu_fetch($key);
}
