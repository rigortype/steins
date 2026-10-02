//! The crate's integration tests, compiled as one binary (xtask's `test_layout` guard).

mod arg_shapes;
mod array_lowering;
mod binding_presence;
mod cast_value;
mod closures;
mod deep_nesting;
mod docblock_assoc;
mod dynamic_call_var;
mod foreach_sites;
mod global_constants;
mod grouped_use;
mod guard_lowering;
mod interpolation_value;
mod isset_value;
mod loop_nesting;
mod method_call_value;
mod operator_sites;
mod operator_value;
mod relative_names;
mod site_oracle;
mod smoke;
mod spread_argument_flatten;
mod terminality;
mod trace_stmt_lowering;
mod unset_seed_presence;
