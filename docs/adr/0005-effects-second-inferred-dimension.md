# Effects: a second inferred dimension, declarations as envelopes, closed origins

Effects replicate the type system's structure (ADR-0001/0002) instead of
importing an effect-handler language model. Effect *polymorphism* is obtained
by call-site propagation, not annotation syntax: at `array_map($cb, $arr)` the
analyzer sees `$cb`'s body and propagates its effects the same way it
propagates types. Koka/Flix-style effect rows and PHPStan's conditional
annotations (`@pure-unless-callable-impure`) are prosthetics for modular
analysis; Steins needs neither.

Declarations (`@throws`, `@pure`, future effect tags) are **effect
envelopes** — upper-bound contracts ("this function does not exceed this
effect set"). Writing one opts that function into always-on envelope checking;
inference exceeding a declared envelope is a finding. No annotation, no check —
consistent with annotation restraint.

**Interface-mediated effects**: PHP culture already abstracts effects behind
interfaces — PSR-20's `ClockInterface` isolates the time/nondeterminism
effect; PSR-3 loggers, Symfony Process, and similar framework seams do the
same. DI-typed call sites are the true residue of opacity (the implementation
is bound at runtime, invisible to call-site propagation), so interfaces act as
**envelope carriers**: an effect envelope declared on an interface method is
what call sites typed against the interface assume; implementations are
checked not to exceed it (Liskov for effects — purer implementations like a
frozen test clock are always legal, since an envelope is an upper bound).
Undeclared interface + unknown implementation widens to unknown-effect. The
effect catalog therefore extends beyond builtins to well-known ecosystem
interfaces.

**Origin closure**: in PHP, primitive effects arise only from language
constructs (`echo`, `exit`, `throw`, `global`, superglobals, `include`/`eval`)
and extension-module/FFI functions. Userland code can only combine these, so
propagating the **builtin effect catalog** covers everything. Uncatalogued
extension functions widen to unknown-effect — the safe side, mirroring folding.
