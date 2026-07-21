//! PHP sidecar IPC — the resident helper process running the project's own PHP
//! that executes real PHP calls for literal folding (CONTEXT.md: "PHP sidecar",
//! "Folding").
//!
//! This crate is an intentional stub for the first vertical-slice milestone. The
//! slice emits only the **sound subset** (CONTEXT.md) — diagnostics provable
//! without executing PHP — so the sidecar is not yet spawned. It exists so the
//! crate boundary is fixed from day one.
//!
//! Design is governed by:
//! - ADR-0004 (PHP sidecar, default-on)
//! - ADR-0024 (sidecar protocol)

// Deliberately empty until ADR-0004/0024 land.
