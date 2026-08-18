//! `ymx-core` — the pure YMX compiler.
//!
//! `ymx-core` is I/O-free: it contains no filesystem, network, or environment
//! access. All types are pure value types. The only entry points are
//! [`compile`] and [`compile_component`], which accept a [`Project`] (built by
//! `ymx-lib::load_project`) and [`Options`], and return a resolved [`Value`]
//! or a list of [`Diagnostic`]s.
//!
//! ## Modules
//!
//! - [`builtin`] — `$map`, `$reduce`, `$merge` special-form implementations
//! - [`callsite`] — inline call-site (`$name(...)`) parsing and resolution
//! - [`diag`] — [`Diagnostic`] type and error-code constants (`E001`…`E013`, `E015`)
//! - [`interp`] — interpolation engine (`$name`, `$N`, `${...}`, escapes)
//! - [`ir`] — [`Value`] IR type (the compiler's internal output representation)
//! - [`math`] — math expression evaluator (`${...}`)
//! - [`namespace`] — namespace registry and lookup
//! - [`parse`] — YAML document parser (produces spanned nodes)
//! - [`project`] — [`Project`] and [`Options`] types
//! - [`resolve`] — rule resolver: rules 1–16 (component resolution pipeline)
//!
//! ## Public API
//!
//! The re-exports in `ymx-lib` expose the full public surface:
//! [`Value`], [`Diagnostic`], [`Options`], [`Format`], [`Project`],
//! [`compile`], [`compile_component`], [`Args`].

pub mod builtin;
pub(crate) mod callsite;
pub mod diag;
pub mod interp;
pub mod ir;
pub mod math;
pub mod namespace;
pub mod parse;
pub mod project;
pub mod resolve;
