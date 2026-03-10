//! MOS → esp-wifi 调度器适配层

extern crate alloc;

use alloc::boxed::Box;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

use esp_wifi::preempt::Scheduler;

use mos::config::*;
use mos::kernel::tcb::CURRENT_TCB_PTR;
use mos::kernel::task::Task;

/// RTOS 适配层就绪标志
pub static RTOS_ADAPTER_READY: AtomicBool = AtomicBool::new(false);

// ============================================================
// Fat Handle — 防止 BLE blob TCB 损坏
// ============================================================

const FAT_HANDLE_SIZE: usize = 512;

#[repr(C, align(4))]
struct FatTaskHandle {
    _opaque: [u8; FAT_HANDLE_SIZE - 8],
    mos_tid: usize,
    thread_sema: u32,
}

static mut FAT_HANDLES: [*mut FatTaskHandle; TASK_MAX] = [core::ptr::null_mut(); TASK_MAX];

fn get_or_create_fat_handle(tid: usize) -> *mut FatTaskHandle {
    unsafe {
        if FAT_HANDLES[tid].is_null() {
            let fh = Box::new(FatTaskHandle {
                _opaque: [0u8; FAT_HANDLE_SIZE - 8],
                mos_tid: tid,
                thread_sema: 0,
            });
            FAT_HANDLES[tid] = Box::into_raw(fh);
        }
        FAT_HANDLES[tid]
    }
}

static mut DUMMY_HANDLE: FatTaskHandle = FatTaskHandle {
    _opaque: [0u8; FAT_HANDLE_SIZE - 8],
    mos_tid: usize::MAX,
    thread_sema: 0,
};

// ============================================================
// Scheduler 实现
// ============================================================

pub struct MosScheduler;

unsafe impl Send for MosScheduler {}
unsafe impl Sync for MosScheduler {}

impl Scheduler for MosScheduler {
    fn enable(&self) {
        RTOS_ADAPTER_READY.store(true, Ordering::Release);
        mos::kprintln!("[rtos] scheduler enabled");
    }

    fn disable(&self) {
        RTOS_ADAPTER_READY.store(false, Ordering::Release);
        mos::kprintln!("[rtos] scheduler disabled");
    }

    fn yield_task(&self) {
        unsafe {
            if CURRENT_TCB_PTR.is_null() {
                return;
            }
        }
        mos::arch::riscv::yield_task();
    }

    fn current_task(&self) -> *mut c_void {
        unsafe {
            if CURRENT_TCB_PTR.is_null() {
                &raw mut DUMMY_HANDLE as *mut c_void
            } else {
                let tid = (*CURRENT_TCB_PTR).tid;
                get_or_create_fat_handle(tid) as *mut c_void
            }
        }
    }

    fn task_create(
        &self,
        task: extern "C" fn(*mut c_void),
        param: *mut c_void,
        task_stack_size: usize,
    ) -> *mut c_void {
        let stack_size = task_stack_size.max(4096);

        let (priority, name): (u8, &str) = if task_stack_size >= 8192 {
            (4, "blob/tmr")
        } else {
            (2, "blob/ctrl")
        };

        mos::kprintln!("[rtos] task_create stack={} pri={}", stack_size, priority);

        match Task::create_raw(task, param, priority, name, stack_size) {
            Ok(handle) => {
                let tcb = handle as *mut mos::kernel::tcb::Tcb;
                let tid = unsafe { (*tcb).tid };
                get_or_create_fat_handle(tid) as *mut c_void
            }
            Err(_) => {
                mos::kprintln!("[rtos] task_create FAILED");
                core::ptr::null_mut()
            }
        }
    }

    fn schedule_task_deletion(&self, task_handle: *mut c_void) {
        if task_handle.is_null() {
            return;
        }
        let fh = task_handle as *mut FatTaskHandle;
        let tid = unsafe { (*fh).mos_tid };
        if tid < TASK_MAX {
            mos::kprintln!("[rtos] task_delete tid={}", tid);
            critical_section::with(|_| unsafe {
                Task::terminate(tid);
                FAT_HANDLES[tid] = core::ptr::null_mut();
            });
            let _ = unsafe { Box::from_raw(fh) };
        }
    }

    fn current_task_thread_semaphore(&self) -> *mut c_void {
        unsafe {
            if CURRENT_TCB_PTR.is_null() {
                static mut DUMMY_SEMA: u32 = 0;
                &raw mut DUMMY_SEMA as *mut c_void
            } else {
                let tid = (*CURRENT_TCB_PTR).tid;
                let fh = get_or_create_fat_handle(tid);
                &raw mut (*fh).thread_sema as *mut c_void
            }
        }
    }
}

esp_wifi::scheduler_impl!(static MOS_SCHED: MosScheduler = MosScheduler);
