//! Trap 分发函数 — 由汇编 trap handler 调用
//!
//! 直接调用板级 BSP 函数，无函数指针间接调用。
//! 与 esp32c6-template/src/arch/riscv.rs 中的 _mos_trap_dispatch 逻辑完全一致。

use core::sync::atomic::Ordering;
use mos::arch::riscv::{YIELD_FROM_ISR, IN_ISR};

use crate::bsp;

/// 中断诊断计数器
#[unsafe(no_mangle)]
pub static mut TRAP_INT_COUNT: u32 = 0;
#[unsafe(no_mangle)]
pub static mut TRAP_LAST_MCAUSE: u32 = 0;
#[unsafe(no_mangle)]
pub static mut TRAP_SYSTICK_COUNT: u32 = 0;

/// 判断 CPU_INT 是否在 esp-hal PRIORITY_TO_INTERRUPT 表中
#[inline(always)]
fn is_esp_hal_cpu_int(code: u32) -> bool {
    matches!(code, 1 | 2 | 5 | 6 | 9..=19)
}

/// 转发非 MOS 中断到 esp-hal 注册的 handler
#[inline(always)]
unsafe fn dispatch_esp_hal_interrupt(cpu_int: u32) {
    unsafe extern "C" {
        fn handle_interrupts(cpu_intr: u32);
    }
    handle_interrupts(cpu_int);
}

/// Trap 分发函数 — 由汇编 trap handler 调用
///
/// 返回值: 1 = 需要上下文切换, 0 = 不需要
#[unsafe(no_mangle)]
unsafe extern "C" fn _mos_trap_dispatch(mcause: u32, frame: *mut u32) -> u32 {
    let is_interrupt = mcause & 0x8000_0000 != 0;
    let code = mcause & 0x7FFF_FFFF;

    // 标记进入中断上下文 (仅对外设中断，ecall 不算)
    if is_interrupt {
        IN_ISR.store(true, Ordering::Release);
    }

    if !is_interrupt {
        // ===== 异常处理 =====
        if code == 11 {
            // M-mode ecall: 用于 yield_task()
            // 将保存的 mepc 前进 4 字节跳过 ecall 指令
            let saved_mepc = core::ptr::read_volatile(frame);
            core::ptr::write_volatile(frame, saved_mepc + 4);
            return 1; // 需要上下文切换
        }
        // ===== WiFi blob 引起的可恢复异常 =====
        let fault_mepc = core::ptr::read_volatile(frame);
        let fault_ra = core::ptr::read_volatile(frame.add(1));  // ra = x1, offset 4

        // 场景 A: 跳转到无效地址 (mepc < 0x40000000)
        if (code == 1 || code == 2) && fault_mepc < 0x4000_0000 {
            core::ptr::write_volatile(frame, fault_ra);
            return 0;
        }

        // 场景 B: 外设时钟被关导致 SYSTIMER 访问 fault 或 flash 读取垃圾
        if code == 1 || code == 2 || code == 5 || code == 7 {
            let repaired = bsp::recover_peripheral_clocks(fault_mepc);
            if repaired {
                return 0;
            }
        }

        // 无法恢复的异常: 输出诊断信息后死循环
        let fault_mepc = core::ptr::read_volatile(frame);
        mos::kprintln!("FAULT: mcause=0x{:08x} mepc=0x{:08x}",
            mcause, fault_mepc);
        mos::kprintln!("  sp(frame)=0x{:08x} ra=0x{:08x}",
            frame as u32, core::ptr::read_volatile(frame.add(1)));
        mos::kprintln!("  a0=0x{:08x} a5=0x{:08x} s2=0x{:08x}",
            core::ptr::read_volatile(frame.add(9)),   // a0 at offset 36
            core::ptr::read_volatile(frame.add(14)),   // a5 at offset 56
            core::ptr::read_volatile(frame.add(17)));  // s2 at offset 68
        // 打印 PCR 时钟状态辅助诊断
        mos::kprintln!("  PCR: systimer=0x{:08x} mspi=0x{:08x} cache=0x{:08x}",
            core::ptr::read_volatile(0x6009_6054 as *const u32),
            core::ptr::read_volatile(0x6009_6018 as *const u32),
            core::ptr::read_volatile(0x6009_6104 as *const u32));
        if fault_mepc >= 0x4000_0000 && fault_mepc < 0x4400_0000 {
            let aligned = fault_mepc & !1u32;
            let instr = core::ptr::read_volatile(aligned as *const u16) as u32;
            mos::kprintln!("  instr@mepc=0x{:04x} mstatus=0x{:08x}",
                instr, core::ptr::read_volatile(frame.add(31)));
        }
        // 打印 __EXTERNAL_INTERRUPTS 表中 BT_MAC 条目 (entry 4)
        {
            unsafe extern "C" {
                static __EXTERNAL_INTERRUPTS: [u32; 77];
            }
            let bt_mac_entry = core::ptr::read_volatile(
                (&raw const __EXTERNAL_INTERRUPTS as *const u32).add(4)
            );
            mos::kprintln!("  EXT_INT[4]=0x{:08x} (BT_MAC handler)", bt_mac_entry);
        }
        loop { core::arch::asm!("wfi"); }
    }

    // ===== 中断处理 =====
    core::ptr::write_volatile(
        &raw mut TRAP_INT_COUNT,
        core::ptr::read_volatile(&raw const TRAP_INT_COUNT).wrapping_add(1),
    );
    core::ptr::write_volatile(&raw mut TRAP_LAST_MCAUSE, mcause);

    let mut ret = if code == bsp::SYSTICK_CPU_INT {
        // ===== MOS Systick 中断 =====
        let missed = bsp::systick_isr();

        core::ptr::write_volatile(
            &raw mut TRAP_SYSTICK_COUNT,
            core::ptr::read_volatile(&raw const TRAP_SYSTICK_COUNT).wrapping_add(1 + missed),
        );

        // 补偿被跳过的 tick (WiFi 延迟导致 ISR 晚执行)
        let mut r = 0u32;
        for _ in 0..=missed {
            r |= mos::kernel::sched::systick_tick();
        }

        r
    } else if is_esp_hal_cpu_int(code) {
        // ===== WiFi 等已注册中断 → 转发到 esp-hal handler =====
        dispatch_esp_hal_interrupt(code);

        // WiFi blob 处理后恢复 systick 整条链路
        bsp::ensure_systimer_clock();
        let mxint_enable = 0x2000_1000 as *mut u32;
        let en = core::ptr::read_volatile(mxint_enable);
        if en & (1 << bsp::SYSTICK_CPU_INT) == 0 {
            core::ptr::write_volatile(mxint_enable, en | (1 << bsp::SYSTICK_CPU_INT));
        }

        0
    } else {
        // ===== 未知 CPU_INT (如 31=WIFI_BB "trash") → 清除并禁用，防止 panic =====
        let mxint_clear = 0x2000_1008 as *mut u32;
        core::ptr::write_volatile(mxint_clear, 1 << code);

        let mxint_enable = 0x2000_1000 as *mut u32;
        let en = core::ptr::read_volatile(mxint_enable);
        core::ptr::write_volatile(mxint_enable, en & !(1 << code));

        // 确保 systick 和 SYSTIMER 时钟仍然启用
        bsp::ensure_systimer_clock();
        let en = core::ptr::read_volatile(mxint_enable);
        if en & (1 << bsp::SYSTICK_CPU_INT) == 0 {
            core::ptr::write_volatile(mxint_enable, en | (1 << bsp::SYSTICK_CPU_INT));
        }

        0
    };

    // 检查 yield_from_isr 延迟标志
    if YIELD_FROM_ISR.swap(false, Ordering::Acquire) {
        ret = 1;
    }

    // 离开中断上下文
    IN_ISR.store(false, Ordering::Release);

    ret
}

/// 系统软件复位 — 板级实现
#[unsafe(no_mangle)]
fn _mos_board_reboot() -> ! {
    esp_hal::system::software_reset()
}
