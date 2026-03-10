//! 格式化打印层 — 抽象接口
//!
//! 板级 crate 通过 register_print_fn() 注册实际输出后端 (RTT, UART 等)。
//! kprint!/kprintln! 宏在内核和板级代码中均可使用。

use core::fmt;

/// 输出后端函数指针 — 板级 crate 注册
static mut PRINT_FN: Option<fn(fmt::Arguments)> = None;

/// 输入后端函数指针 — 板级 crate 注册 (非阻塞读取一个字节)
static mut GETCHAR_FN: Option<fn() -> Option<u8>> = None;

/// 注册打印输出后端
pub fn register_print_fn(f: fn(fmt::Arguments)) {
    unsafe { PRINT_FN = Some(f); }
}

/// 注册输入后端
pub fn register_getchar_fn(f: fn() -> Option<u8>) {
    unsafe { GETCHAR_FN = Some(f); }
}

/// 非阻塞读取输入
#[inline]
pub fn kgetchar_nonblock() -> Option<u8> {
    unsafe {
        if let Some(f) = GETCHAR_FN {
            f()
        } else {
            None
        }
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    unsafe {
        if let Some(f) = PRINT_FN {
            f(args);
        }
    }
}

/// 格式化打印 (不换行)
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::print::_print(::core::format_args!($($arg)*))
    };
}

/// 格式化打印 (换行)
#[macro_export]
macro_rules! kprintln {
    () => {
        $crate::print::_print(::core::format_args!("\n"))
    };
    ($($arg:tt)*) => {{
        $crate::print::_print(::core::format_args!($($arg)*));
        $crate::print::_print(::core::format_args!("\n"));
    }};
}