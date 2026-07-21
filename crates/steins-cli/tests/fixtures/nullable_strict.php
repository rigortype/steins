<?php

declare(strict_types=1);

function maybe(?int $n): ?int {
    return $n;
}

maybe(null);
maybe(5);
