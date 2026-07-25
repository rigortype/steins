<?php

// Array literals as fold arguments (issue #39). `count`, `in_array` and
// `implode` sat on the `foldable` allowlist unable to qualify until an array
// literal could cross the fold seam; this fixture is the end-to-end proof that
// they now fire, through the real allowlist and the project's own PHP.

$n = count([1, 2, 3]);
$joined = implode(",", ["a", "b"]);
$member = in_array(2, [1, 2, 3]);

// Nested literals are represented, not widened: count() sees two entries.
$nested = count([[1, 2], [3]]);

// PHP's own key rules decide these, because PHP builds the array: a duplicate
// key is one entry, and an absent key follows the largest int key seen.
$dup = count([1 => 'a', 1 => 'b']);
$mixed = implode(",", ['x' => 'a', 5 => 'b', 'c']);

// An element that is not a proven value widens the WHOLE array: $x may hold
// anything, so the literal's length is not the array's length.
$widened = count([1, $x]);
