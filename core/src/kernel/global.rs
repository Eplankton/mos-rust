//! 内核全局状态管理

use crate::config::*;
use crate::kernel::list::TaskList;
use crate::kernel::tcb::{TaskStatus, Tcb};

// ==========================================
// 1. 全局任务内存池
// ==========================================

pub static mut TCBS: [Tcb; TASK_MAX] = [const { Tcb::empty(0) }; TASK_MAX];
pub static mut STACKS: [[u32; PAGE_SIZE_WORDS]; TASK_MAX] = [[0; PAGE_SIZE_WORDS]; TASK_MAX];

// ==========================================
// 2. 核心调度队列
// ==========================================

pub static mut READY_LIST: TaskList = TaskList::new();
pub static mut BLOCKED_LIST: TaskList = TaskList::new();
pub static mut SLEEPING_LIST: TaskList = TaskList::new();

// ==========================================
// 3. 系统时钟与标识
// ==========================================

pub static mut OS_TICKS: u32 = 0;

#[inline]
pub fn os_ticks() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const OS_TICKS) }
}

/// Volatile 写入 OS_TICKS — 防止 LTO 优化掉中断中的写操作
#[inline]
pub unsafe fn set_os_ticks(val: u32) {
    core::ptr::write_volatile(&raw mut OS_TICKS, val);
}

pub static mut USER_NAME: [u8; USER_NAME_SIZE] = *b"neo\0\0\0\0\0";

// ==========================================
// 4. Tid 分配器
// ==========================================

pub static mut TID_BITMAP: u32 = 0;

#[inline]
pub unsafe fn alloc_tid() -> Option<usize> {
    for tid in 0..TASK_MAX {
        if (TID_BITMAP & (1 << tid)) == 0 {
            TID_BITMAP |= 1 << tid;
            TCBS[tid] = Tcb::empty(tid);
            return Some(tid);
        }
    }
    None
}

#[inline]
pub unsafe fn free_tid(tid: usize) {
    if tid < TASK_MAX {
        TID_BITMAP &= !(1 << tid);
        TCBS[tid].status = TaskStatus::Terminated;
    }
}