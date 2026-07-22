<?php

// A caller in a nested directory: render() is defined in ../lib.php. Project
// mode resolves it cross-file and proves the literal TypeError.
render("abc");
