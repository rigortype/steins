# Scope floor: PHP 8.x only, 8.1 as the working floor

Steins implements for PHP 8.x only; PHP 7 is permanently out of scope
(PHPStan already serves legacy; a 7.x compat layer is the kind of debt that
kills a greenfield). The working floor is **8.1**: enums are load-bearing for
effect attributes (ADR-0006) and as the output target of flagship transforms
(DTO promotion, stringly→enum, ADR-0010), and the native-declaration
philosophy presumes 8.x typing features. Supporting 8.0 may be worth a cost
estimate later; not now. The lossless parser reads older syntax anyway —
quality guarantees (diagnostics, transforms) apply from 8.1 up.
