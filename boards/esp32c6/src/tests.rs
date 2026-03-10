//! 测试任务集

use mos::config::*;
use mos::kernel::Task;
use mos::kernel::global::os_ticks;
use mos::kernel::sync::Mutex;
use mos::kernel::ipc::MsgQueue;
use crate::lprintln;

// ==========================================
// 1. MutexTest: 优先级继承 (PIP) 测试
// ==========================================

static PIP_MUTEX: Mutex<()> = Mutex::new(());

const PRI_H: u8 = 1;
const PRI_M: u8 = 2;
const PRI_L: u8 = 3;

fn pip_task_l() {
    lprintln!("[L] Trying to lock...");
    {
        let _guard = PIP_MUTEX.lock();
        lprintln!("[L] Locked. Busy waiting 500ms for H/M...");
        let start = os_ticks();
        while os_ticks() < start.wrapping_add(500) {
            unsafe { core::arch::asm!("nop") };
        }
        lprintln!("[L] Working (M should NOT preempt)...");
        for _ in 0..3 { busy_delay(100); }
        lprintln!("[L] Unlock");
    }
    lprintln!("[L] Finished");
}

fn pip_task_h() {
    Task::delay(100);
    lprintln!("[H] Started! Trying to lock (Should boost L)...");
    {
        let _guard = PIP_MUTEX.lock();
        lprintln!("[H] Got Lock!");
    }
    lprintln!("[H] Finished");
}

fn pip_task_m() {
    Task::delay(200);
    lprintln!("[M] Started.");
    lprintln!("[M] Done! (Should be LAST)");
}

fn pip_test_root() {
    lprintln!("\n=== [Test] PIP Inversion Test (L/M/H) ===");
    let _ = Task::create(pip_task_h, PRI_H, "mtx/H");
    let _ = Task::create(pip_task_m, PRI_M, "mtx/M");
    let _ = Task::create(pip_task_l, PRI_L, "mtx/L");
}

pub fn mutex_test() {
    let _ = Task::create(pip_test_root, PRI_MIN, "pip_root");
}

// ==========================================
// 2. MsgQueueTest
// ==========================================

static MSG_CHANNEL: MsgQueue<u32, 3> = MsgQueue::new();

fn msg_producer() {
    let mut i: u32 = 0;
    loop {
        let _ = MSG_CHANNEL.send(i, 0);
        i = i.wrapping_add(1);
        Task::delay(50);
    }
}

fn msg_consumer() {
    loop {
        match MSG_CHANNEL.recv(200) {
            Ok(_msg) => {}
            Err(_) => { mos::kprintln!("MsgQ Timeout!"); }
        }
    }
}

fn msgq_launch() {
    const BASE_PRI: u8 = 3;
    let _ = Task::create(msg_consumer, BASE_PRI, "msg_q/recv");
    for p in BASE_PRI..=(BASE_PRI * 2) {
        let _ = Task::create(msg_producer, p, "msg_q/send");
    }
}

pub fn msgq_test() {
    let _ = Task::create(msgq_launch, PRI_MAX, "msg_q/test");
}

fn busy_delay(ms: u32) {
    let start = os_ticks();
    while os_ticks() < start.wrapping_add(ms) {
        unsafe { core::arch::asm!("nop") };
    }
}

#[cfg(feature = "async")]
use mos::kernel::async_rt::AsyncRuntime;

// ==========================================
// 3. AsyncTest
// ==========================================

#[cfg(feature = "async")]
static mut ASYNC_REMAINING: usize = 0;

#[cfg(feature = "async")]
pub fn async_test(num: usize) {
    let mut count = 0usize;
    for k in (1..=num).rev() {
        if AsyncRuntime::spawn(async_sum(k)) {
            count += 1;
        }
    }
    unsafe { ASYNC_REMAINING = count; }
    if count < num {
        mos::kprintln!("[async] spawned {}/{} (pool full)", count, num);
    }
}

#[cfg(feature = "async")]
async fn async_sum(n: usize) {
    let mut result: f32 = 0.0;
    let mut rng: u32 = (n as u32).wrapping_mul(63641362).wrapping_add(1);

    for i in 0..n {
        result += i as f32;
        rng = rng.wrapping_mul(63641365).wrapping_add(os_ticks());
        if n > 0 && (rng as usize % n) == 0 {
            let delay_ms = 1000 + 10 * (rng % 13);
            AsyncRuntime::delay(delay_ms).await;
        }
    }

    let final_result = result * 1.234;
    lprintln!("sum({}) => {}", n, final_result as u32);

    unsafe {
        ASYNC_REMAINING -= 1;
        if ASYNC_REMAINING == 0 {
            lprintln!("All Async Sum Works are Done!");
        }
    }
}
