//! 架构抽象层

#[cfg(target_arch = "riscv32")]
pub mod riscv;
#[cfg(target_arch = "riscv32")]
pub use riscv::*;