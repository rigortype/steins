//! The crate's integration tests, compiled as one binary (xtask's `test_layout` guard).

mod array_lowering;
mod binding_presence;
mod cast_value;
mod closures;
mod deep_nesting;
mod docblock_assoc;
mod foreach_sites;
mod global_constants;
mod grouped_use;
mod isset_value;
mod method_call_value;
mod operator_value;
mod relative_names;
mod smoke;
mod spread_argument_flatten;
mod terminality;
mod unset_seed_presence;
