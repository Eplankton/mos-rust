//! 任务同步原语 — Semaphore, Mutex (PIP), CondVar, Barrier

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use critical_section;

use crate::config::PRI_INV;
use crate::kernel::global::*;
use crate::kernel::list::TaskList;
use crate::kernel::tcb::{TaskStatus, CURRENT_TCB_PTR};
use crate::arch::riscv::yield_task;

// ==========================================
// 1. 信号量
// ==========================================

pub struct Sema {
    inner: UnsafeCell<SemaInner>,
}

struct SemaInner {
    count: i32,
    wait_queue: TaskList,
}

unsafe impl Sync for Sema {}

impl Sema {
    pub const fn new(initial_count: i32) -> Self {
        Self {
            inner: UnsafeCell::new(SemaInner {
                count: initial_count,
                wait_queue: TaskList::new(),
            }),
        }
    }

    pub fn down(&self) {
        critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            inner.count -= 1;
            if inner.count < 0 {
                let cr_tid = (*CURRENT_TCB_PTR).tid;
                TCBS[cr_tid].status = TaskStatus::Blocked;
                READY_LIST.remove(cr_tid);
                let _ = inner.wait_queue.insert_in_order(cr_tid, |t| {
                    TCBS[cr_tid].priority < TCBS[t].priority
                });
                yield_task();
            }
        });
    }

    pub fn up(&self) {
        critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            inner.count += 1;
            if inner.count <= 0 {
                if let Some(woken_tid) = inner.wait_queue.pop_front() {
                    TCBS[woken_tid].status = TaskStatus::Ready;
                    let _ = READY_LIST.insert_in_order(woken_tid, |t| {
                        TCBS[woken_tid].priority < TCBS[t].priority
                    });
                    if !CURRENT_TCB_PTR.is_null() {
                        if TCBS[woken_tid].priority < (*CURRENT_TCB_PTR).priority {
                            yield_task();
                        }
                    }
                }
            }
        });
    }

    /// 非阻塞尝试获取信号量
    pub fn try_down(&self) -> bool {
        critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            if inner.count > 0 {
                inner.count -= 1;
                true
            } else {
                false
            }
        })
    }

    /// 带超时获取信号量 (ticks 为 0 等同 try_down)
    pub fn down_timeout(&self, ticks: u32) -> bool {
        // 先尝试非阻塞获取
        if self.try_down() {
            return true;
        }
        if ticks == 0 {
            return false;
        }

        // 阻塞当前任务并加入等待队列
        critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            inner.count -= 1;
            let cr_tid = (*CURRENT_TCB_PTR).tid;
            TCBS[cr_tid].status = TaskStatus::Blocked;
            READY_LIST.remove(cr_tid);
            let _ = inner.wait_queue.insert_in_order(cr_tid, |t| {
                TCBS[cr_tid].priority < TCBS[t].priority
            });

            // 设置超时唤醒
            let wake_time = os_ticks().wrapping_add(ticks);
            TCBS[cr_tid].wake_point = wake_time;
            let _ = SLEEPING_LIST.insert_in_order(cr_tid, |current_tid| {
                wake_time < TCBS[current_tid].wake_point
            });
            yield_task();
        });

        // 醒来后检查是否成功获取 (被 up() 唤醒 vs 超时唤醒)
        critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            let cr_tid = (*CURRENT_TCB_PTR).tid;
            let still_waiting = inner.wait_queue.iter().any(|&t| t == cr_tid);
            if still_waiting {
                // 超时: 从等待队列移除，恢复 count
                inner.wait_queue.remove(cr_tid);
                inner.count += 1;
                false
            } else {
                // 成功获取
                true
            }
        })
    }

    /// 获取当前信号量计数
    pub fn count(&self) -> i32 {
        critical_section::with(|_| unsafe {
            (*self.inner.get()).count
        })
    }

    pub fn up_from_isr(&self) {
        critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            inner.count += 1;
            if inner.count <= 0 {
                if let Some(woken_tid) = inner.wait_queue.pop_front() {
                    TCBS[woken_tid].status = TaskStatus::Ready;
                    let _ = READY_LIST.insert_in_order(woken_tid, |t| {
                        TCBS[woken_tid].priority < TCBS[t].priority
                    });
                }
            }
        });
    }
}

// ==========================================
// 2. 互斥锁 (Mutex 带 PIP)
// ==========================================

pub struct Mutex<T> {
    inner: UnsafeCell<MutexInner>,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

struct MutexInner {
    owner: Option<usize>,
    recursive_count: u32,
    wait_queue: TaskList,
}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            inner: UnsafeCell::new(MutexInner {
                owner: None,
                recursive_count: 0,
                wait_queue: TaskList::new(),
            }),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        critical_section::with(|_| unsafe {
            self.internal_lock();
        });
        MutexGuard { mutex: self }
    }

    unsafe fn internal_lock(&self) {
        let inner = &mut *self.inner.get();
        let cr_tid = (*CURRENT_TCB_PTR).tid;

        if inner.owner == Some(cr_tid) {
            inner.recursive_count += 1;
            return;
        }

        if let Some(owner_tid) = inner.owner {
            let cur_pri = TCBS[cr_tid].priority;
            if cur_pri < TCBS[owner_tid].priority {
                if TCBS[owner_tid].sub_priority == PRI_INV {
                    TCBS[owner_tid].sub_priority = TCBS[owner_tid].priority;
                }
                TCBS[owner_tid].priority = cur_pri;
                if TCBS[owner_tid].status == TaskStatus::Ready {
                    READY_LIST.remove(owner_tid);
                    let _ = READY_LIST.insert_in_order(owner_tid, |t| {
                        TCBS[owner_tid].priority < TCBS[t].priority
                    });
                }
            }
            TCBS[cr_tid].status = TaskStatus::Blocked;
            READY_LIST.remove(cr_tid);
            let _ = inner.wait_queue.insert_in_order(cr_tid, |t| {
                TCBS[cr_tid].priority < TCBS[t].priority
            });
            yield_task();
        } else {
            inner.owner = Some(cr_tid);
            inner.recursive_count = 1;
        }
    }

    unsafe fn internal_unlock(&self) {
        let inner = &mut *self.inner.get();
        let cr_tid = (*CURRENT_TCB_PTR).tid;

        inner.recursive_count -= 1;
        if inner.recursive_count > 0 { return; }

        if TCBS[cr_tid].sub_priority != PRI_INV {
            TCBS[cr_tid].priority = TCBS[cr_tid].sub_priority;
            TCBS[cr_tid].sub_priority = PRI_INV;
        }

        inner.owner = None;

        if let Some(woken_tid) = inner.wait_queue.pop_front() {
            inner.owner = Some(woken_tid);
            inner.recursive_count = 1;
            TCBS[woken_tid].status = TaskStatus::Ready;
            let _ = READY_LIST.insert_in_order(woken_tid, |t| {
                TCBS[woken_tid].priority < TCBS[t].priority
            });
            if TCBS[woken_tid].priority < TCBS[cr_tid].priority {
                yield_task();
            }
        } else {
            if let Some(front_tid) = READY_LIST.front() {
                if TCBS[front_tid].priority < TCBS[cr_tid].priority {
                    yield_task();
                }
            }
        }
    }
}

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        critical_section::with(|_| unsafe {
            self.mutex.internal_unlock();
        });
    }
}

// ==========================================
// 3. 条件变量
// ==========================================

pub struct CondVar {
    wait_queue: UnsafeCell<TaskList>,
}

unsafe impl Sync for CondVar {}

impl CondVar {
    pub const fn new() -> Self {
        Self { wait_queue: UnsafeCell::new(TaskList::new()) }
    }

    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        let mutex = guard.mutex;
        critical_section::with(|_| unsafe {
            let cr_tid = (*CURRENT_TCB_PTR).tid;
            let wq = &mut *self.wait_queue.get();
            let _ = wq.push_back(cr_tid);
            TCBS[cr_tid].status = TaskStatus::Blocked;
            READY_LIST.remove(cr_tid);
            mutex.internal_unlock();
            core::mem::forget(guard);
            yield_task();
        });
        mutex.lock()
    }

    pub fn notify(&self) {
        critical_section::with(|_| unsafe {
            let wq = &mut *self.wait_queue.get();
            if let Some(woken_tid) = wq.pop_front() {
                TCBS[woken_tid].status = TaskStatus::Ready;
                let _ = READY_LIST.insert_in_order(woken_tid, |t| {
                    TCBS[woken_tid].priority < TCBS[t].priority
                });
                yield_task();
            }
        });
    }

    pub fn notify_all(&self) {
        critical_section::with(|_| unsafe {
            let wq = &mut *self.wait_queue.get();
            let mut woken = false;
            while let Some(woken_tid) = wq.pop_front() {
                TCBS[woken_tid].status = TaskStatus::Ready;
                let _ = READY_LIST.insert_in_order(woken_tid, |t| {
                    TCBS[woken_tid].priority < TCBS[t].priority
                });
                woken = true;
            }
            if woken { yield_task(); }
        });
    }
}

// ==========================================
// 4. 屏障
// ==========================================

struct BarrierState {
    count: u32,
    generation: u32,
}

pub struct Barrier {
    mutex: Mutex<BarrierState>,
    cv: CondVar,
    total: u32,
}

impl Barrier {
    pub const fn new(total: u32) -> Self {
        Self {
            mutex: Mutex::new(BarrierState { count: 0, generation: 0 }),
            cv: CondVar::new(),
            total,
        }
    }

    pub fn wait(&self) {
        let mut guard = self.mutex.lock();
        let my_gen = guard.generation;
        guard.count += 1;

        if guard.count == self.total {
            guard.count = 0;
            guard.generation = guard.generation.wrapping_add(1);
            drop(guard);
            self.cv.notify_all();
        } else {
            loop {
                guard = self.cv.wait(guard);
                if guard.generation != my_gen { break; }
            }
        }
    }
}
