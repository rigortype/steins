<?php

// Third-party (vendor) code: height() is defined here and called badly here.
// This finding lands on a path with a `/vendor/` component, so it is suppressed
// by default (ADR-0015) and only shown under --vendor-diagnostics.
function height(int $h): int
{
    return $h;
}

height("xyz");
