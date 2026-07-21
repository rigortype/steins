<?php

declare(strict_types=1);

function width(int $w): int {
    return $w;
}

function area(float $a): float {
    return $a;
}

width("5");
width(5);
width(5.0);
area(5);
