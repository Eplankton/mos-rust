//! 任务间通信 — 消息队列

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use critical_section;

use crate::kernel::global::*;
use crate::kernel::list::TaskList;
use crate::kernel::tcb::{TaskStatus, CURRENT_TCB_PTR};
use crate::kernel::task::Task;
use crate::arch::riscv::yield_task;

#[derive(Debug, PartialEq, Eq)]
pub enum IpcStatus {
    Ok,
    TimeOut,
}

struct MsgQueueInner<T, const N: usize> {
    buffer: [MaybeUninit<T>; N],
    head: usize,
    tail: usize,
    len: usize,
    senders: TaskList,
    receivers: TaskList,
}

impl<T: Copy, const N: usize> MsgQueueInner<T, N> {
    const fn new() -> Self {
        Self {
            buffer: [const { MaybeUninit::uninit() }; N],
            head: 0, tail: 0, len: 0,
            senders: TaskList::new(),
            receivers: TaskList::new(),
        }
    }

    fn push(&mut self, msg: T) {
        if self.len < N {
            self.buffer[self.tail] = MaybeUninit::new(msg);
            self.tail = (self.tail + 1) % N;
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<T> {
        if self.len > 0 {
            let msg = unsafe { self.buffer[self.head].assume_init_read() };
            self.head = (self.head + 1) % N;
            self.len -= 1;
            Some(msg)
        } else {
            None
        }
    }
}

pub struct MsgQueue<T, const N: usize> {
    inner: UnsafeCell<MsgQueueInner<T, N>>,
}

unsafe impl<T: Send + Copy, const N: usize> Sync for MsgQueue<T, N> {}

impl<T: Copy, const N: usize> MsgQueue<T, N> {
    pub const fn new() -> Self {
        Self { inner: UnsafeCell::new(MsgQueueInner::new()) }
    }

    unsafe fn wait_on(wait_list: &mut TaskList, timeout_ticks: u32) -> IpcStatus {
        if timeout_ticks == 0 { return IpcStatus::TimeOut; }

        critical_section::with(|_| {
            let cr_tid = (*CURRENT_TCB_PTR).tid;
            let _ = wait_list.insert_in_order(cr_tid, |t| {
                TCBS[cr_tid].priority < TCBS[t].priority
            });
        });

        Task::delay(timeout_ticks);

        critical_section::with(|_| {
            let cr_tid = (*CURRENT_TCB_PTR).tid;
            let still_waiting = wait_list.iter().any(|&t| t == cr_tid);
            if still_waiting {
                wait_list.remove(cr_tid);
                IpcStatus::TimeOut
            } else {
                IpcStatus::Ok
            }
        })
    }

    unsafe fn try_wake_up(wait_list: &mut TaskList) {
        if let Some(woken_tid) = wait_list.pop_front() {
            TCBS[woken_tid].wake_point = 0xFFFF_FFFF;
            TCBS[woken_tid].status = TaskStatus::Ready;
            SLEEPING_LIST.remove(woken_tid);
            let _ = READY_LIST.insert_in_order(woken_tid, |t| {
                TCBS[woken_tid].priority < TCBS[t].priority
            });
            if !CURRENT_TCB_PTR.is_null() {
                let cr_tid = (*CURRENT_TCB_PTR).tid;
                if TCBS[woken_tid].priority < TCBS[cr_tid].priority {
                    yield_task();
                }
            }
        }
    }

    pub fn send(&self, msg: T, timeout: u32) -> Result<(), IpcStatus> {
        let is_full = critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            if inner.len < N {
                inner.push(msg);
                Self::try_wake_up(&mut inner.receivers);
                false
            } else {
                true
            }
        });

        if !is_full { return Ok(()); }

        let status = unsafe {
            let inner = &mut *self.inner.get();
            Self::wait_on(&mut inner.senders, timeout)
        };

        if status == IpcStatus::TimeOut { return Err(IpcStatus::TimeOut); }

        critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            if inner.len < N {
                inner.push(msg);
                Self::try_wake_up(&mut inner.receivers);
                Ok(())
            } else {
                Err(IpcStatus::TimeOut)
            }
        })
    }

    pub fn recv(&self, timeout: u32) -> Result<T, IpcStatus> {
        let msg = critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            if inner.len > 0 {
                let msg = inner.pop();
                Self::try_wake_up(&mut inner.senders);
                msg
            } else {
                None
            }
        });

        if let Some(msg) = msg { return Ok(msg); }

        let status = unsafe {
            let inner = &mut *self.inner.get();
            Self::wait_on(&mut inner.receivers, timeout)
        };

        if status == IpcStatus::TimeOut { return Err(IpcStatus::TimeOut); }

        critical_section::with(|_| unsafe {
            let inner = &mut *self.inner.get();
            if let Some(msg) = inner.pop() {
                Self::try_wake_up(&mut inner.senders);
                Ok(msg)
            } else {
                Err(IpcStatus::TimeOut)
            }
        })
    }
}
