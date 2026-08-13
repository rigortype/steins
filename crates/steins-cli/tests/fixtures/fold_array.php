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

// An element that is not a proven value keeps the FOLD from running — the
// seam sends the real engine a real array or nothing (ADR-0028 §2) — but the
// entry count never depended on the element: `[1, $x]` has two entries
// whatever $x holds (probed: count([1, $x]) === 2 for every $x). So the fold
// declines and the shape rung answers, which is issue #327's whole point.
$unfolded = count([1, $x]);

// A SPREAD is the case that really is unknowable: `...$x` contributes as many
// entries as $x has, so the literal's length is not the array's length and the
// whole literal drops to no fact at all.
$widened = count([1, ...$x]);
