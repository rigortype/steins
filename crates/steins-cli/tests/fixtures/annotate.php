<?php
declare(strict_types=1);

final class Box
{
}

function price(): string
{
    return "abc";
}

function width(int $w): int
{
    return $w;
}

function writer(): void
{
    file_put_contents("/tmp/x", "y");
}

function mystery(): void
{
    some_uncatalogued_builtin();
}

$upper = strtoupper("xy");
$named = price();
$count = 42;
$box = new Box();
width("nope");
