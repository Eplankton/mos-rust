//! 异步执行器 — 基于 Rust async/await 的静态嵌入式运行时

use core::future::Future;
use core::mem::MaybeUninit;
use core::pin::Pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use critical_section;

use crate::config::*;
use crate::kernel::global::os_ticks;
use crate::kernel::task::Task;


// ==========================================
// 1. 任务槽
// ==========================================

type PollFn = unsafe fn(storage: *mut u8, cx: &mut Context<'_>) -> Poll<()>;
type DropFn = unsafe fn(storage: *mut u8);

#[repr(C, align(4))]
struct AlignedStorage([u8; ASYNC_FRAME_SIZE]);

struct TaskSlot {
    poll_fn: Option<PollFn>,
    drop_fn: Option<DropFn>,
    ready: bool,
    storage: AlignedStorage,
}

impl TaskSlot {
    const fn empty() -> Self {
        Self {
            poll_fn: None, drop_fn: None, ready: false,
            storage: AlignedStorage([0u8; ASYNC_FRAME_SIZE]),
        }
    }

    #[inline]
    fn is_free(&self) -> bool { self.poll_fn.is_none() }

    unsafe fn store<F: Future<Output = ()> + 'static>(&mut self, future: F) {
        let ptr = self.storage.0.as_mut_ptr() as *mut F;
        ptr.write(future);
        self.poll_fn = Some(poll_erased::<F>);
        self.drop_fn = if core::mem::needs_drop::<F>() {
            Some(drop_erased::<F>)
        } else {
            None
        };
        self.ready = true;
    }

    unsafe fn clear(&mut self) {
        if let Some(drop_fn) = self.drop_fn.take() {
            drop_fn(self.storage.0.as_mut_ptr());
        }
        self.poll_fn = None;
        self.ready = false;
    }
}

unsafe fn poll_erased<F: Future<Output = ()>>(storage: *mut u8, cx: &mut Context<'_>) -> Poll<()> {
    let future = &mut *(storage as *mut F);
    Pin::new_unchecked(future).poll(cx)
}

unsafe fn drop_erased<F>(storage: *mut u8) {
    core::ptr::drop_in_place(storage as *mut F);
}

// ==========================================
// 2. Waker
// ==========================================

static WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
    waker_clone, waker_wake, waker_wake_by_ref, waker_drop,
);

unsafe fn waker_clone(data: *const ()) -> RawWaker {
    RawWaker::new(data, &WAKER_VTABLE)
}

unsafe fn waker_wake(data: *const ()) { waker_wake_by_ref(data); }

unsafe fn waker_wake_by_ref(data: *const ()) {
    let idx = data as usize;
    critical_section::with(|_| {
        if idx < ASYNC_POOL_MAX {
            unsafe { TASK_POOL[idx].ready = true; }
        }
    });
}

unsafe fn waker_drop(_data: *const ()) {}

fn make_waker(idx: usize) -> Waker {
    unsafe { Waker::from_raw(RawWaker::new(idx as *const (), &WAKER_VTABLE)) }
}

// ==========================================
// 3. 定时器队列
// ==========================================

struct TimerEntry {
    wake_tick: u32,
    waker: Waker,
}

struct TimerQueue {
    data: [MaybeUninit<TimerEntry>; ASYNC_POOL_MAX],
    len: usize,
}

impl TimerQueue {
    const fn new() -> Self {
        Self {
            data: [const { MaybeUninit::uninit() }; ASYNC_POOL_MAX],
            len: 0,
        }
    }

    fn push(&mut self, entry: TimerEntry) {
        if self.len >= ASYNC_POOL_MAX { return; }
        self.data[self.len] = MaybeUninit::new(entry);
        self.len += 1;
        self.sift_up(self.len - 1);
    }

    unsafe fn wake_expired(&mut self) {
        let now = os_ticks();
        while self.len > 0 {
            let top = &*self.data[0].as_ptr();
            if (now.wrapping_sub(top.wake_tick) as i32) >= 0 {
                let entry = self.pop_front();
                entry.waker.wake();
            } else {
                break;
            }
        }
    }

    fn pop_front(&mut self) -> TimerEntry {
        unsafe {
            let top = self.data[0].as_ptr().read();
            self.len -= 1;
            if self.len > 0 {
                let last = self.data[self.len].as_ptr().read();
                self.data[0] = MaybeUninit::new(last);
                self.sift_down(0);
            }
            top
        }
    }

    fn sift_up(&mut self, mut idx: usize) {
        while idx > 0 {
            let parent = (idx - 1) / 2;
            unsafe {
                let idx_tick = (*self.data[idx].as_ptr()).wake_tick;
                let parent_tick = (*self.data[parent].as_ptr()).wake_tick;
                if (idx_tick.wrapping_sub(parent_tick) as i32) < 0 {
                    let tmp = self.data[idx].as_ptr().read();
                    let tmp_p = self.data[parent].as_ptr().read();
                    self.data[idx] = MaybeUninit::new(tmp_p);
                    self.data[parent] = MaybeUninit::new(tmp);
                    idx = parent;
                } else {
                    break;
                }
            }
        }
    }

    fn sift_down(&mut self, mut idx: usize) {
        loop {
            let left = 2 * idx + 1;
            let right = 2 * idx + 2;
            let mut smallest = idx;
            unsafe {
                if left < self.len {
                    let s = (*self.data[smallest].as_ptr()).wake_tick;
                    let l = (*self.data[left].as_ptr()).wake_tick;
                    if (l.wrapping_sub(s) as i32) < 0 { smallest = left; }
                }
                if right < self.len {
                    let s = (*self.data[smallest].as_ptr()).wake_tick;
                    let r = (*self.data[right].as_ptr()).wake_tick;
                    if (r.wrapping_sub(s) as i32) < 0 { smallest = right; }
                }
            }
            if smallest != idx {
                unsafe {
                    let tmp = self.data[idx].as_ptr().read();
                    let tmp_s = self.data[smallest].as_ptr().read();
                    self.data[idx] = MaybeUninit::new(tmp_s);
                    self.data[smallest] = MaybeUninit::new(tmp);
                }
                idx = smallest;
            } else {
                break;
            }
        }
    }
}

// ==========================================
// 4. 全局状态
// ==========================================

static mut TASK_POOL: [TaskSlot; ASYNC_POOL_MAX] = [const { TaskSlot::empty() }; ASYNC_POOL_MAX];
static mut TIMERS: TimerQueue = TimerQueue::new();
static mut INIT_FLAG: bool = false;

// ==========================================
// 5. 执行器
// ==========================================

fn ensure_init() {
    unsafe {
        if INIT_FLAG { return; }
        if Task::create(executor_task, 1, "async/exec").is_ok() {
            INIT_FLAG = true;
        }
    }
}

fn executor_task() {
    crate::kprintln!("[async/exec] started");
    loop {
        let did_work = poll_all();
        if !did_work {
            Task::delay(1);
        }
    }
}

fn poll_all() -> bool {
    critical_section::with(|_| unsafe { TIMERS.wake_expired(); });

    let mut did_work = false;
    for idx in 0..ASYNC_POOL_MAX {
        unsafe {
            if TASK_POOL[idx].is_free() || !TASK_POOL[idx].ready { continue; }
            TASK_POOL[idx].ready = false;
            did_work = true;
            let poll_fn = TASK_POOL[idx].poll_fn.unwrap();
            let waker = make_waker(idx);
            let mut cx = Context::from_waker(&waker);
            let result = poll_fn(TASK_POOL[idx].storage.0.as_mut_ptr(), &mut cx);
            if result.is_ready() { TASK_POOL[idx].clear(); }
        }
    }
    did_work
}

// ==========================================
// 6. 公共 API
// ==========================================

pub struct AsyncRuntime;

impl AsyncRuntime {
    pub fn spawn<F: Future<Output = ()> + 'static>(future: F) -> bool {
        const { assert!(core::mem::size_of::<F>() <= ASYNC_FRAME_SIZE, "Future too large for task slot") };
        const { assert!(core::mem::align_of::<F>() <= core::mem::align_of::<usize>()) };

        ensure_init();

        critical_section::with(|_| unsafe {
            for idx in 0..ASYNC_POOL_MAX {
                if TASK_POOL[idx].is_free() {
                    TASK_POOL[idx].store(future);
                    return true;
                }
            }
            false
        })
    }

    pub fn delay(ms: u32) -> Timer {
        Timer { wake_tick: os_ticks().wrapping_add(ms), registered: false }
    }

    pub fn yield_now() -> YieldOnce {
        YieldOnce { yielded: false }
    }
}

// ==========================================
// 7. Future 实现
// ==========================================

pub struct Timer {
    wake_tick: u32,
    registered: bool,
}

impl Future for Timer {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let now = os_ticks();
        if (now.wrapping_sub(self.wake_tick) as i32) >= 0 {
            return Poll::Ready(());
        }
        if !self.registered {
            self.registered = true;
            let waker = cx.waker().clone();
            critical_section::with(|_| unsafe {
                TIMERS.push(TimerEntry { wake_tick: self.wake_tick, waker });
            });
        }
        Poll::Pending
    }
}

pub struct YieldOnce {
    yielded: bool,
}

impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}
