//! 内核调度器 — RISC-V 版本

use crate::config::*;
use crate::kernel::global::*;
use crate::kernel::tcb::{TaskStatus, CURRENT_TCB_PTR};
use crate::arch::riscv::start_first_task;
use crate::kernel::task::Task;

/// 空闲任务
fn idle_task() {
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}

/// 调度器是否已准备好
#[unsafe(no_mangle)]
pub static mut SCHED_READY: bool = false;

pub struct Scheduler;

impl Scheduler {
    /// 启动调度器 (永不返回)
    pub fn launch() -> ! {
        Task::create(idle_task, PRI_MIN, "idle")
            .expect("Failed to create idle task");

        unsafe {
            assert!(!READY_LIST.is_empty(), "OS Launch Failed: No ready task!");

            let first_tid = READY_LIST.front().unwrap();
            TCBS[first_tid].status = TaskStatus::Running;
            CURRENT_TCB_PTR = &mut TCBS[first_tid] as *mut _;

            SCHED_READY = true;

            crate::arch::riscv::install_trap_handler();
            start_first_task();
        }
    }
}

/// 处理睡眠队列，唤醒到期任务
unsafe fn wake_up_sleepers() {
    let now = os_ticks();
    let mut i = 0;
    while i < SLEEPING_LIST.len() {
        let tid = SLEEPING_LIST.iter().nth(i).copied().unwrap();
        if now >= TCBS[tid].wake_point {
            SLEEPING_LIST.remove(tid);
            TCBS[tid].status = TaskStatus::Ready;
            TCBS[tid].wake_point = 0;
            let priority = TCBS[tid].priority;
            let _ = READY_LIST.insert_in_order(tid, |current_tid| {
                priority < TCBS[current_tid].priority
            });
        } else {
            i += 1;
        }
    }
}

/// 选择下一个任务 — 由 trap handler 汇编调用
#[unsafe(no_mangle)]
pub unsafe extern "C" fn next_tcb() {
    wake_up_sleepers();

    let cr_tid = (*CURRENT_TCB_PTR).tid;

    if TCBS[cr_tid].status == TaskStatus::Terminated || TCBS[cr_tid].status == TaskStatus::Blocked {
        READY_LIST.remove(cr_tid);
    } else if TCBS[cr_tid].time_slice == 0 || TCBS[cr_tid].status == TaskStatus::Running {
        TCBS[cr_tid].time_slice = TIME_SLICE;
        TCBS[cr_tid].status = TaskStatus::Ready;
        READY_LIST.remove(cr_tid);
        let priority = TCBS[cr_tid].priority;
        let _ = READY_LIST.insert_in_order(cr_tid, |current_tid| {
            priority < TCBS[current_tid].priority
        });
    }

    if let Some(st_tid) = READY_LIST.front() {
        if st_tid != cr_tid {
            if TCBS[cr_tid].status == TaskStatus::Running {
                TCBS[cr_tid].status = TaskStatus::Ready;
            }
            TCBS[st_tid].status = TaskStatus::Running;
            CURRENT_TCB_PTR = &mut TCBS[st_tid] as *mut _;
        }
    } else {
        panic!("System halted: No ready task (not even idle)!");
    }
}

/// SysTick 处理 — 由 trap dispatch 调用
/// 返回 1 表示需要上下文切换，0 表示不需要
#[unsafe(no_mangle)]
pub unsafe fn systick_tick() -> u32 {
    if !SCHED_READY {
        return 0;
    }

    set_os_ticks(os_ticks().wrapping_add(1));

    let cr_tid = (*CURRENT_TCB_PTR).tid;
    let cur_tcb = &mut TCBS[cr_tid];

    if cur_tcb.time_slice > 0 {
        cur_tcb.time_slice -= 1;
    }

    // 仅在时间片耗尽或有睡眠任务可能到期时才触发调度
    if cur_tcb.time_slice == 0 || !SLEEPING_LIST.is_empty() {
        1
    } else {
        0
    }
}