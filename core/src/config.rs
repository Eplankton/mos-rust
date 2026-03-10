//! 内核全局配置 (等同于 mos-renode/core/config.hpp 和 macro.hpp)

#![allow(dead_code)]

// ==========================================
// 调度与系统配置
// ==========================================
/// ESP32C6 无硬件浮点
pub const USE_HARD_FPU: bool = false;

/// 系统滴答定时器频率配置 (1000 = 1ms)
pub const SYSTICK_HZ: u32 = 1000;

/// 最大支持的任务数量
pub const TASK_MAX: usize = 16;

/// 默认的页/栈大小 (单位：Bytes)
pub const PAGE_SIZE: usize = 1024;
/// 转换为字(Words, 32-bit)的长度，用于栈分配
pub const PAGE_SIZE_WORDS: usize = PAGE_SIZE / 4;

/// 时间片大小 (单位：ticks)
pub const TIME_SLICE: u32 = 50;

/// 任务闭包内联存储大小 (字节)
pub const TASK_CLOSURE_SIZE: usize = 32;

// ==========================================
// 优先级配置 (0 为最高优先级)
// ==========================================
pub const PRI_MAX: u8 = 0;
pub const PRI_MIN: u8 = 127;
/// 用于表示无效优先级的特殊标记
pub const PRI_INV: u8 = 255;

// ==========================================
// Shell 配置
// ==========================================
pub const SHELL_BUF_SIZE: usize = 32;
pub const SHELL_USR_CMD_SIZE: usize = 8;
pub const USER_NAME_SIZE: usize = 8;

// ==========================================
// 异步执行器配置 (对应 C++ async.hpp)
// ==========================================
/// Lambda 捕获对象的字节大小
pub const ASYNC_TASK_SIZE: usize = 32;
/// 最大并行协程数量 (每个占 ~84B 静态内存, BLE blob 需要大量 SRAM)
pub const ASYNC_POOL_MAX: usize = 16;
/// 协程帧大小 (字节)
pub const ASYNC_FRAME_SIZE: usize = 64;
