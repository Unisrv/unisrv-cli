//! Per-type diff and render logic.
//!
//! Each submodule owns the field-by-field knowledge for one config type.
//! All field accesses go through struct destructuring patterns so adding a
//! field to the underlying type produces a compile error in every diff site
//! that hasn't been updated to handle it.

pub mod deployment;
pub mod service;
