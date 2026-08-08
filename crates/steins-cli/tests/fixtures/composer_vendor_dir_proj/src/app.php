<?php

// First-party code: width() is defined here and called badly here.
function width(int $w): int
{
    return $w;
}

width("abc");
