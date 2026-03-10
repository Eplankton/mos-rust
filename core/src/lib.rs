#![no_std]
#![allow(dead_code)]
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(static_mut_refs)]

extern crate alloc;

pub mod config;
pub mod print;
pub mod arch;
pub mod kernel;
pub mod data_type;