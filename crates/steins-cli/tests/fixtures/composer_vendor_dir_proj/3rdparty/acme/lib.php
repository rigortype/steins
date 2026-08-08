<?php

// Third-party code installed under the declared `3rdparty` vendor-dir (issue
// #181). Not literally `vendor/`, so this finding is suppressed by default only
// because the vendor answer reads `config.vendor-dir` from composer.json.
function height(int $h): int
{
    return $h;
}

height("xyz");
