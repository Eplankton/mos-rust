//! 任务创建与生命周期管理 — RISC-V 版本

extern crate alloc;

use core::ffi::c_void;
use critical_section;
use crate::config::*;
use crate::kernel::global::*;
use crate::kernel::tcb::{init_stack_frame, init_stack_frame_raw, TaskStatus, CURRENT_TCB_PTR};
use crate::kernel::sched::SCHED_READY;
use crate::arch::riscv::yield_task;

/// 任务退出桩函数
fn task_exit_stub() {
    Task::exit_current();
}

/// 闭包蹦床函数
fn closure_trampoline(_: *mut ()) {
    unsafe {
        let tcb = &mut *CURRENT_TCB_PTR;
        if let Some(invoker) = tcb.closure_invoker.take() {
            invoker(tcb.closure_storage.as_mut_ptr());
        }
    }
}

/// 类型擦除的闭包调用函数
unsafe fn invoke_closure<F: FnOnce()>(ptr: *mut u8) {
    let f = (ptr as *mut F).read();
    f();
}

pub struct Task;

impl Task {
    /// 终止当前任务
    pub fn exit_current() -> ! {
        critical_section::with(|_| {
            unsafe {
                let cr_tid = (*CURRENT_TCB_PTR).tid;
                READY_LIST.remove(cr_tid);
                free_tid(cr_tid);
                yield_task();
            }
        });
        loop { unsafe { core::arch::asm!("nop") }; }
    }

    /// 创建新任务
    pub fn create<F: FnOnce() + 'static>(
        f: F,
        priority: u8,
        name: &'static str,
    ) -> Result<usize, &'static str> {
        const { assert!(core::mem::size_of::<F>() <= TASK_CLOSURE_SIZE, "Closure too large for TCB storage") };
        const { assert!(core::mem::align_of::<F>() <= core::mem::align_of::<usize>()) };

        critical_section::with(|_| {
            unsafe {
                let tid = alloc_tid().ok_or("Task pool is full")?;

                let tcb = &mut TCBS[tid];
                let ptr = tcb.closure_storage.as_mut_ptr() as *mut F;
                ptr.write(f);
                tcb.closure_invoker = Some(invoke_closure::<F>);

                let stack = &mut STACKS[tid];
                let sp = init_stack_frame(stack, closure_trampoline, core::ptr::null_mut(), task_exit_stub);

                tcb.sp = sp;
                tcb.status = TaskStatus::Ready;
                tcb.priority = priority;
                tcb.time_slice = TIME_SLICE;
                tcb.name = name;
                tcb.stack_bottom = stack.as_ptr() as u32;

                let _ = READY_LIST.insert_in_order(tid, |current_tid| {
                    priority < TCBS[current_tid].priority
                });

                if SCHED_READY && !CURRENT_TCB_PTR.is_null() {
                    let current_pri = (*CURRENT_TCB_PTR).priority;
                    if priority < current_pri {
                        yield_task();
                    }
                }

                Ok(tid)
            }
        })
    }

    /// 创建任务 — 接受 C 函数指针 (WiFi blob 用)
    ///
    /// 与 create() 不同:
    /// - 入口是 extern "C" fn(*mut c_void) + param
    /// - 栈从堆分配 (stack_size 字节)
    /// - 返回 TCB 指针 (作为 task handle)
    pub fn create_raw(
        entry: extern "C" fn(*mut c_void),
        param: *mut c_void,
        priority: u8,
        name: &'static str,
        stack_size: usize,
    ) -> Result<*mut c_void, &'static str> {
        let stack_words = stack_size / 4;
        // 从堆分配栈
        let stack_buf = alloc::vec![0u32; stack_words].leak();

        critical_section::with(|_| {
            unsafe {
                let tid = alloc_tid().ok_or("Task pool is full")?;

                let tcb = &mut TCBS[tid];
                let sp = init_stack_frame_raw(
                    stack_buf,
                    entry as *const () as u32,
                    param as u32,
                    task_exit_stub as *const () as u32,
                );

                tcb.sp = sp;
                tcb.status = TaskStatus::Ready;
                tcb.priority = priority;
                tcb.time_slice = TIME_SLICE;
                tcb.name = name;
                tcb.stack_bottom = stack_buf.as_ptr() as u32;
                tcb.heap_stack_base = stack_buf.as_ptr() as u32;

                let _ = READY_LIST.insert_in_order(tid, |current_tid| {
                    priority < TCBS[current_tid].priority
                });

                // 不立即 yield — 让 systick 在下一个 tick 自然抢占。
                // BLE blob 在 BleConnector::new() 中创建高优先级任务,
                // 立即 yield 会导致 blob busy-loop 饿死调用者。
                // systick 抢占延迟 (≤1ms) 对所有场景可接受。

                Ok(&mut TCBS[tid] as *mut _ as *mut c_void)
            }
        })
    }

    /// 按名称查找任务
    pub fn find_tid(name: &str) -> Option<usize> {
        critical_section::with(|_| unsafe {
            for tid in 0..TASK_MAX {
                if TCBS[tid].status != TaskStatus::Terminated && TCBS[tid].name == name {
                    return Some(tid);
                }
            }
            None
        })
    }

    /// 阻塞任务
    pub fn block(tid: usize) {
        critical_section::with(|_| unsafe {
            match TCBS[tid].status {
                TaskStatus::Ready | TaskStatus::Running => {
                    TCBS[tid].status = TaskStatus::Blocked;
                    READY_LIST.remove(tid);
                    let _ = BLOCKED_LIST.push_back(tid);
                    yield_task();
                }
                _ => {}
            }
        });
    }

    /// 恢复任务
    pub fn resume(tid: usize) {
        critical_section::with(|_| unsafe {
            if TCBS[tid].status != TaskStatus::Blocked { return; }
            let in_blocked_list = BLOCKED_LIST.iter().any(|&t| t == tid);
            if !in_blocked_list { return; }
            BLOCKED_LIST.remove(tid);
            TCBS[tid].status = TaskStatus::Ready;
            let priority = TCBS[tid].priority;
            let _ = READY_LIST.insert_in_order(tid, |t| priority < TCBS[t].priority);
            if !CURRENT_TCB_PTR.is_null() && priority < (*CURRENT_TCB_PTR).priority {
                yield_task();
            }
        });
    }

    /// 终止任务
    pub fn terminate(tid: usize) {
        critical_section::with(|_| unsafe {
            if TCBS[tid].status == TaskStatus::Terminated { return; }
            READY_LIST.remove(tid);
            SLEEPING_LIST.remove(tid);
            BLOCKED_LIST.remove(tid);
            free_tid(tid);
            yield_task();
        });
    }

    /// 延时
    pub fn delay(ticks: u32) {
        if ticks == 0 { return; }
        critical_section::with(|_| {
            unsafe {
                let cr_tid = (*CURRENT_TCB_PTR).tid;
                let wake_time = os_ticks().wrapping_add(ticks);
                TCBS[cr_tid].wake_point = wake_time;
                TCBS[cr_tid].status = TaskStatus::Blocked;
                READY_LIST.remove(cr_tid);
                let _ = SLEEPING_LIST.insert_in_order(cr_tid, |current_tid| {
                    wake_time < TCBS[current_tid].wake_point
                });
                yield_task();
            }
        });
    }
}

/// 创建任务的便捷宏
#[macro_export]
macro_rules! task {
    ($entry:expr, $pri:expr, $name:expr) => {
        $crate::kernel::task::Task::create($entry, $pri, $name)
            .expect(concat!("Failed to create task '", $name, "'"))
    };
}
