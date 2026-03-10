//! RISC-V 架构特定代码 — 上下文切换与中断处理
//!
//! 使用自定义 trap handler (direct mode) 实现:
//! - ecall: 用于主动上下文切换 (yield_task / start_first_task)
//! - Timer 中断: 用于抢占式调度 (systick)
//!
//! 上下文帧布局 (32 words = 128 bytes):
//!   Offset  Register
//!   0       mepc
//!   4       ra  (x1)
//!   8       gp  (x3)
//!   12      tp  (x4)
//!   16      t0  (x5)
//!   20      t1  (x6)
//!   24      t2  (x7)
//!   28      s0  (x8)
//!   32      s1  (x9)
//!   36      a0  (x10)
//!   40      a1  (x11)
//!   44      a2  (x12)
//!   48      a3  (x13)
//!   52      a4  (x14)
//!   56      a5  (x15)
//!   60      a6  (x16)
//!   64      a7  (x17)
//!   68      s2  (x18)
//!   72      s3  (x19)
//!   76      s4  (x20)
//!   80      s5  (x21)
//!   84      s6  (x22)
//!   88      s7  (x23)
//!   92      s8  (x24)
//!   96      s9  (x25)
//!   100     s10 (x26)
//!   104     s11 (x27)
//!   108     t3  (x28)
//!   112     t4  (x29)
//!   116     t5  (x30)
//!   120     t6  (x31)
//!   124     mstatus

use core::arch::{naked_asm, asm};
use core::sync::atomic::{AtomicBool, Ordering};

const ISR_STACK_SIZE: usize = 8192;

/// 上下文帧大小 (32 words = 128 bytes)
/// 30 GP regs (x1, x3-x31) + mepc + mstatus = 32
pub const CONTEXT_WORDS: usize = 32;
pub const FRAME_SIZE: usize = CONTEXT_WORDS * 4; // 128 bytes

/// 专用 ISR 栈 — 防止 WiFi blob ISR 溢出小栈任务 (idle/shell 仅 1KB)
#[repr(align(16))]
struct IsrStack([u8; ISR_STACK_SIZE]);

static mut ISR_STACK: IsrStack = IsrStack([0; ISR_STACK_SIZE]);

/// ISR 栈顶地址 (运行时初始化, 供汇编 trap handler 读取)
#[unsafe(no_mangle)]
static mut ISR_STACK_TOP: usize = 0;

/// 触发上下文切换 — 通过 ecall 异常
#[inline(always)]
pub fn yield_task() {
    unsafe { asm!("ecall") };
}

/// 启动第一个任务 — 从 CURRENT_TCB_PTR 加载上下文并 mret
pub fn start_first_task() -> ! {
    unsafe {
        asm!(
            "la t0, CURRENT_TCB_PTR",
            "lw t1, 0(t0)",
            "lw sp, 0(t1)",

            "lw t0, 124(sp)",
            "ori t0, t0, 0x80",
            "andi t0, t0, -9",
            "csrw mstatus, t0",

            "lw t0, 0(sp)",
            "csrw mepc, t0",

            "lw x1,   4(sp)",
            "lw x3,   8(sp)",
            "lw x4,  12(sp)",
            "lw x5,  16(sp)",
            "lw x6,  20(sp)",
            "lw x7,  24(sp)",
            "lw x8,  28(sp)",
            "lw x9,  32(sp)",
            "lw x10, 36(sp)",
            "lw x11, 40(sp)",
            "lw x12, 44(sp)",
            "lw x13, 48(sp)",
            "lw x14, 52(sp)",
            "lw x15, 56(sp)",
            "lw x16, 60(sp)",
            "lw x17, 64(sp)",
            "lw x18, 68(sp)",
            "lw x19, 72(sp)",
            "lw x20, 76(sp)",
            "lw x21, 80(sp)",
            "lw x22, 84(sp)",
            "lw x23, 88(sp)",
            "lw x24, 92(sp)",
            "lw x25, 96(sp)",
            "lw x26, 100(sp)",
            "lw x27, 104(sp)",
            "lw x28, 108(sp)",
            "lw x29, 112(sp)",
            "lw x30, 116(sp)",
            "lw x31, 120(sp)",

            "addi sp, sp, 128",
            "mret",
            options(noreturn)
        );
    }
}

/// 系统软件复位 — 由板级 crate 提供实际实现
/// 板级 crate 应覆盖此弱符号或直接调用自己的 reboot
pub fn reboot() -> ! {
    // 板级 crate 应提供 _mos_board_reboot 符号
    unsafe extern "C" {
        fn _mos_board_reboot() -> !;
    }
    unsafe { _mos_board_reboot() }
}

/// 安装自定义 trap handler (direct mode)
///
/// 必须在 esp_hal::init() 之后调用
pub unsafe fn install_trap_handler() {
    ISR_STACK_TOP = &ISR_STACK as *const _ as usize + ISR_STACK_SIZE;

    asm!(
        "la t0, _mos_trap_handler",
        "andi t0, t0, -4",
        "csrw mtvec, t0",
    );
}

/// 自定义 trap handler — vectored 跳转表 + 主处理器
#[unsafe(no_mangle)]
#[unsafe(naked)]
#[unsafe(link_section = ".trap")]
unsafe extern "C" fn _mos_trap_handler() {
    naked_asm!(
        ".balign 256",
        ".option push",
        ".option norvc",
        ".rept 32",
        "j 3f",
        ".endr",
        ".option pop",

        "3:",
        "addi sp, sp, -128",

        "sw x1,   4(sp)",
        "sw x3,   8(sp)",
        "sw x4,  12(sp)",
        "sw x5,  16(sp)",
        "sw x6,  20(sp)",
        "sw x7,  24(sp)",
        "sw x8,  28(sp)",
        "sw x9,  32(sp)",
        "sw x10, 36(sp)",
        "sw x11, 40(sp)",
        "sw x12, 44(sp)",
        "sw x13, 48(sp)",
        "sw x14, 52(sp)",
        "sw x15, 56(sp)",
        "sw x16, 60(sp)",
        "sw x17, 64(sp)",
        "sw x18, 68(sp)",
        "sw x19, 72(sp)",
        "sw x20, 76(sp)",
        "sw x21, 80(sp)",
        "sw x22, 84(sp)",
        "sw x23, 88(sp)",
        "sw x24, 92(sp)",
        "sw x25, 96(sp)",
        "sw x26, 100(sp)",
        "sw x27, 104(sp)",
        "sw x28, 108(sp)",
        "sw x29, 112(sp)",
        "sw x30, 116(sp)",
        "sw x31, 120(sp)",

        "csrr t0, mepc",
        "sw t0, 0(sp)",
        "csrr t0, mstatus",
        "sw t0, 124(sp)",

        "la t0, SCHED_READY",
        "lbu t0, 0(t0)",
        "beqz t0, 1f",
        "la t0, CURRENT_TCB_PTR",
        "lw t1, 0(t0)",
        "sw sp, 0(t1)",
        "1:",

        "mv s0, sp",

        "csrr a0, mcause",
        "bgez a0, 6f",
        "la t0, ISR_STACK_TOP",
        "lw sp, 0(t0)",
        "6:",

        "mv a1, s0",
        "call _mos_trap_dispatch",

        "la t0, SCHED_READY",
        "lbu t0, 0(t0)",
        "beqz t0, 8f",
        "beqz a0, 8f",

        "call next_tcb",

        "la t0, CURRENT_TCB_PTR",
        "lw t1, 0(t0)",
        "lw sp, 0(t1)",
        "j 2f",

        "8:",
        "mv sp, s0",
        "2:",

        "lw t0, 124(sp)",
        "ori t0, t0, 0x80",
        "andi t0, t0, -9",
        "csrw mstatus, t0",
        "lw t0, 0(sp)",
        "csrw mepc, t0",

        "lw x1,   4(sp)",
        "lw x3,   8(sp)",
        "lw x4,  12(sp)",
        "lw x5,  16(sp)",
        "lw x6,  20(sp)",
        "lw x7,  24(sp)",
        "lw x8,  28(sp)",
        "lw x9,  32(sp)",
        "lw x10, 36(sp)",
        "lw x11, 40(sp)",
        "lw x12, 44(sp)",
        "lw x13, 48(sp)",
        "lw x14, 52(sp)",
        "lw x15, 56(sp)",
        "lw x16, 60(sp)",
        "lw x17, 64(sp)",
        "lw x18, 68(sp)",
        "lw x19, 72(sp)",
        "lw x20, 76(sp)",
        "lw x21, 80(sp)",
        "lw x22, 84(sp)",
        "lw x23, 88(sp)",
        "lw x24, 92(sp)",
        "lw x25, 96(sp)",
        "lw x26, 100(sp)",
        "lw x27, 104(sp)",
        "lw x28, 108(sp)",
        "lw x29, 112(sp)",
        "lw x30, 116(sp)",
        "lw x31, 120(sp)",

        "addi sp, sp, 128",
        "mret",
    );
}

/// ISR 中请求上下文切换的延迟标志
pub static YIELD_FROM_ISR: AtomicBool = AtomicBool::new(false);

/// 当前是否在中断上下文中 (trap handler 内)
pub static IN_ISR: AtomicBool = AtomicBool::new(false);

/// 从 ISR 中请求上下文切换 (不可在 ISR 中直接 ecall)
pub fn yield_task_from_isr() {
    YIELD_FROM_ISR.store(true, Ordering::Release);
}

