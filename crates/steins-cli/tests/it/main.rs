//! The crate's integration tests, compiled as one binary (xtask's `test_layout` guard).

mod check_fix;
mod cli;
mod deep_nesting;
mod doctor;
mod effect_diff;
mod format_github;
mod format_recorded;
mod format_sarif;
mod generation_capture_root;
mod license;
mod mcp;
mod output_seam;
mod plugin_channel;
mod profile;
mod runtime_final_keyword;
mod sidecar_handshake;
mod suppress;
mod symlink_dedup;
mod symlink_walk;
mod tolerated_effects;
mod transform;
mod transform_loops;
mod triage;
mod vendor_paths_config;
