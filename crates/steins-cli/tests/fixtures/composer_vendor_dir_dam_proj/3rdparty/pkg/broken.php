<?php

// Deliberately broken PHP under the declared `3rdparty` vendor-dir (issue
// #181): parser test suites ship broken PHP on purpose, and the ADR-0046 §2
// vendor presumption must recognize this as vendor from composer's declared
// vendor-dir, not only from a literal `vendor/` component.
function broken( int $x {
    return $x;
}
