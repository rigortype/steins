<?php

declare(strict_types=1);

namespace App;

/**
 * The label `acme/steins-plugin` registers is legal in an envelope: it is a
 * descendant of the plugin's own composer vendor root (ADR-0068 §2), so the
 * registry knows it and `effect.unknown-label` stays quiet.
 */
#[\Steins\Effect('acme.cache')]
function read_cached(string $key): string
{
    return \acme_cache_get($key);
}

/**
 * A caller of the above: `acme.cache` reaches this summary's declared lane by
 * ordinary propagation, with no envelope of its own to import it.
 */
function warm_cache(string $key): string
{
    return read_cached($key);
}

/**
 * A typo of a registered label is still a typo. The suggestion now searches the
 * extended registry, so it can name the plugin's label.
 */
#[\Steins\Effect('acme.cach')]
function read_cached_typo(string $key): string
{
    return \acme_cache_get($key);
}
