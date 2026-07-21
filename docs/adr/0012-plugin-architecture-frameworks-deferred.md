# Rigor's plugin architecture imported; framework support deferred, Laravel eventually first-class

Steins adopts Rigor's extension model: a **plugin** is a fact producer for a
target library or DSL — not part of the inference engine — returning facts,
synthetic declarations, effect-catalog entries, and diagnostics through a
contract the core owns (core stays authoritative; plugins refine, never
weaken). Plugins are written in PHP and hosted in the PHP sidecar
(ADR-0004), kept off the hot path. The decisive advantage over static
mimicry: a sidecar plugin can boot the real framework and *ask it* — read
Laravel's actual container bindings, Doctrine's actual metadata — the same
ask-the-real-thing philosophy as folding, cashing in the sidecar a second
time. Ecosystem effect-catalog entries (PSR-20 class, ADR-0005) ship through
this same channel.

Initial scope defers framework magic deliberately: the corpus and quality
bars are built on framework-free code (the composer library/CLI world) first.
Framework code stays sound via widening, and that silence names itself
through coverage posture (ADR-0009's discipline) — the "not useful on Laravel
yet" reputation risk is accepted in exchange for a quality foundation.
Laravel support is intended to become **first-class** later, through this
plugin channel. PHPStan-extension compatibility (larastan etc.) is not
pursued — the models differ too much.
