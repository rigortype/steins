<?php

function width(int $w): int {
    return $w;
}

$x = $_GET['x'];
width(strtolower($x));
