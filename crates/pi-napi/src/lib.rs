//! Node-API (N-API) v8 implemented over pi-quickjs.
//!
//! Native addons (`.node` files) link against the `napi_*` C symbols at load time. This crate
//! defines that function table for the pi-rs executable, so unchanged addons built against
//! Node's headers run without a V8 dependency.
//!
//! Two layers live here:
//!
//! * the C ABI entry points (`sys` types, `napi_*` functions), and
//! * [`exports::load`], which the pi-jsrt loader calls with a freshly opened library and turns a
//!   register function plus an environment into the module's exports object.
//!
//! Threading: an environment is bound to the thread that created it. Every entry point checks
//! that binding and wraps its body in `catch_unwind`, so a Rust panic never unwinds into addon
//! C code.

// The names below are Node's C API: they must match its headers exactly.
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
// `qjs::size_t` (a C `size_t`) is a distinct type from `usize` even where equal in width.
#![allow(clippy::unnecessary_cast)]

pub mod arraybuffer;
pub mod env;
pub mod error;
pub mod exports;
pub mod function;
pub mod object;
pub mod scope;
pub mod sys;
pub mod value;
pub mod wrap;

pub use exports::{Addon, load};
