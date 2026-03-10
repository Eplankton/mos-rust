//! 任务控制块 (TCB) — RISC-V 版本

use crate::config::*;
use crate::arch::riscv::CONTEXT_WORDS;
use core::fmt;
use core::ptr;

/// 任务运行状态
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum TaskStatus {
    Terminated,
    Ready,
    Running,
    Blocked,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready      => f.pad("Ready"),
            Self::Running    => f.pad("Running"),
            Self::Blocked    => f.pad("Blocked"),
            Self::Terminated => f.pad("Dead"),
        }
    }
}

/// 任务控制块
/// #[repr(C)] 确保 sp 是第一个字段 (偏移量为 0)
#[repr(C)]
pub struct Tcb {
    /// 任务的当前栈指针 (必须是偏移量 0!)
    pub sp: u32,
    pub tid: usize,
    pub status: TaskStatus,
    pub priority: u8,
    /// 备份优先级 (用于 PIP)
    pub sub_priority: u8,
    pub time_slice: u32,
    /// 唤醒时间点
    pub wake_point: u32,
    pub name: &'static str,
    pub stack_bottom: u32,
    /// 闭包内联存储
    pub(crate) closure_storage: [u8; TASK_CLOSURE_SIZE],
    /// 类型擦除的闭包调用函数指针
    pub(crate) closure_invoker: Option<unsafe fn(*mut u8)>,
    /// per-task thread semaphore (WiFi blob 需要, rtos_adapter 使用)
    pub thread_sema: *mut (),
    /// 堆分配的栈指针 (用于 create_raw 创建的任务, 0 = 使用静态栈池)
    pub heap_stack_base: u32,
}

impl Tcb {
    pub const fn empty(tid: usize) -> Self {
        Self {
            sp: 0,
            tid,
            status: TaskStatus::Terminated,
            priority: PRI_MIN,
            sub_priority: PRI_INV,
            time_slice: TIME_SLICE,
            wake_point: 0,
            name: "",
            stack_bottom: 0,
            closure_storage: [0u8; TASK_CLOSURE_SIZE],
            closure_invoker: None,
            thread_sema: core::ptr::null_mut(),
            heap_stack_base: 0,
        }
    }

    #[inline]
    pub fn is_sleeping(&self) -> bool {
        self.wake_point != 0 && self.status == TaskStatus::Blocked
    }
}

/// 全局当前运行任务的 TCB 指针
#[unsafe(no_mangle)]
pub static mut CURRENT_TCB_PTR: *mut Tcb = ptr::null_mut();

/// 初始化 RISC-V 任务栈帧
/// 返回初始栈顶指针 (SP)
pub(crate) fn init_stack_frame(
    stack: &mut [u32],
    entry: fn(*mut ()),
    argv: *mut (),
    exit_fn: fn(),
) -> u32 {
    let stack_len = stack.len();
    assert!(stack_len > CONTEXT_WORDS, "Stack too small for context frame");

    let frame_base = stack_len - CONTEXT_WORDS;

    for i in 0..CONTEXT_WORDS {
        stack[frame_base + i] = 0;
    }

    stack[frame_base + 0] = entry as u32;
    stack[frame_base + 1] = exit_fn as u32;
    stack[frame_base + 9] = argv as u32;
    stack[frame_base + 31] = 0x0000_1888;

    let sp_ptr = &stack[frame_base] as *const u32;
    sp_ptr as u32
}

/// 初始化 RISC-V 任务栈帧 — raw 版本 (用于 C 函数指针)
pub(crate) fn init_stack_frame_raw(
    stack: &mut [u32],
    entry_addr: u32,
    argv: u32,
    exit_addr: u32,
) -> u32 {
    let stack_len = stack.len();
    assert!(stack_len > CONTEXT_WORDS, "Stack too small for context frame");

    let frame_base = stack_len - CONTEXT_WORDS;

    for i in 0..CONTEXT_WORDS {
        stack[frame_base + i] = 0;
    }

    stack[frame_base + 0] = entry_addr;
    stack[frame_base + 1] = exit_addr;
    stack[frame_base + 9] = argv;
    stack[frame_base + 31] = 0x0000_1888;

    let sp_ptr = &stack[frame_base] as *const u32;
    sp_ptr as u32
}