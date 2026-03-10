//! ESP32-C6 板级支持 — Systimer 初始化 + ISR helper
//!
//! 使用 esp-hal API 初始化 SYSTIMER TARGET0 为 1ms 周期中断。
//! ISR handler 封装了 Zephyr set_systimer_alarm() 的寄存器操作序列。
//!
//! 对外仅导出:
//!   - `SYSTICK_CPU_INT` — CPU 中断号，供 trap 层判断中断源
//!   - `init_systimer()` — 初始化函数
//!   - `systick_isr()` — ISR 中调用的硬件处理函数

use esp_hal::interrupt::{self, CpuInterrupt, InterruptKind, Priority};
use esp_hal::system::Cpu;
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::timer::Timer;

/// Systick 使用的 CPU 中断号（BSP 导出，trap 层引用）
///
/// 必须使用 esp-hal PRIORITY_TO_INTERRUPT 表之外的 CPU_INT，否则
/// esp-hal 的 interrupt::enable() (WiFi 等) 会覆盖路由。
/// 表中使用: 1,2,5,6,9-19。CPU_INT_20 不在表中且不被硬件保留。
pub const SYSTICK_CPU_INT: u32 = 20;

/// Systick 周期: 1ms @ 16MHz (XTAL 40MHz 经 4/10 分频)
const SYSTIMER_PERIOD: u32 = 16_000;

/// 初始化 SYSTIMER TARGET0 为 1ms 周期中断
pub fn init_systimer(systimer: esp_hal::peripherals::SYSTIMER) {
    // 阶段 1: 中断路由
    unsafe {
        interrupt::map(
            Cpu::current(),
            esp_hal::peripherals::Interrupt::SYSTIMER_TARGET0,
            CpuInterrupt::Interrupt20,
        );
        interrupt::set_priority(
            Cpu::current(),
            CpuInterrupt::Interrupt20,
            Priority::Priority3,
        );
    }
    interrupt::set_kind(Cpu::current(), CpuInterrupt::Interrupt20, InterruptKind::Edge);
    interrupt::clear(Cpu::current(), CpuInterrupt::Interrupt20);

    unsafe { enable_cpu_int_and_mie(); }

    // 阶段 2+3: SYSTIMER 配置 + 首次闹钟
    let syst = SystemTimer::new(systimer);
    let alarm0 = syst.alarm0;

    alarm0.enable_auto_reload(false);
    alarm0.clear_interrupt();

    let _ = alarm0.load_value(esp_hal::time::Duration::from_micros(1000));

    alarm0.start();
    alarm0.enable_interrupt(true);
}

/// 启用 CPU 中断 (PLIC_MX enable + mie CSR)
unsafe fn enable_cpu_int_and_mie() {
    const PLIC_MX_BASE: u32 = 0x2000_1000;
    const MXINT_ENABLE: *mut u32 = PLIC_MX_BASE as *mut u32;

    let en = core::ptr::read_volatile(MXINT_ENABLE);
    core::ptr::write_volatile(MXINT_ENABLE, en | (1 << SYSTICK_CPU_INT));

    core::arch::asm!(
        "csrrs zero, mie, {0}",
        in(reg) (1u32 << 11),
    );
}

/// 重新确保 systick 整条链路 (SYSTIMER → PLIC → MIE) 都正常
#[inline]
pub(crate) unsafe fn ensure_systick_enabled() -> bool {
    let mut fixed = ensure_systimer_clock();

    const MXINT_ENABLE: *mut u32 = 0x2000_1000 as *mut u32;
    let en = core::ptr::read_volatile(MXINT_ENABLE);
    if en & (1 << SYSTICK_CPU_INT) == 0 {
        core::ptr::write_volatile(MXINT_ENABLE, en | (1 << SYSTICK_CPU_INT));
        fixed = true;
    }

    let int_ena = core::ptr::read_volatile(regs::INT_ENA);
    if int_ena & 0x1 == 0 {
        core::ptr::write_volatile(regs::INT_ENA, int_ena | 0x1);
        fixed = true;
    }

    let conf = core::ptr::read_volatile(regs::CONF);
    if conf & (1u32 << 24) == 0 {
        core::ptr::write_volatile(regs::UNIT0_OP, 1u32 << 30);
        while core::ptr::read_volatile(regs::UNIT0_OP as *const u32) & (1 << 29) == 0 {}
        let cur_lo = core::ptr::read_volatile(regs::UNIT0_VALUE_LO);
        let cur_hi = core::ptr::read_volatile(regs::UNIT0_VALUE_HI);
        let (next_lo, carry) = cur_lo.overflowing_add(SYSTIMER_PERIOD);
        let next_hi = if carry { (cur_hi + 1) & 0xFFFFF } else { cur_hi };
        core::ptr::write_volatile(regs::TARGET0_LO, next_lo);
        core::ptr::write_volatile(regs::TARGET0_HI, next_hi);
        core::ptr::write_volatile(regs::COMP0_LOAD, 1);
        core::ptr::write_volatile(regs::CONF, conf | (1u32 << 24));
        fixed = true;
    }

    core::arch::asm!("csrsi mstatus, 0x8", options(nomem, nostack));

    fixed
}

// ============ ISR Helpers ============

mod regs {
    pub const BASE: u32 = 0x6000_a000;
    pub const CONF: *mut u32              = (BASE + 0x00) as _;
    pub const UNIT0_OP: *mut u32          = (BASE + 0x04) as _;
    pub const UNIT0_VALUE_LO: *const u32  = (BASE + 0x08) as _;
    pub const UNIT0_VALUE_HI: *const u32  = (BASE + 0x0C) as _;
    pub const TARGET0_HI: *mut u32        = (BASE + 0x1C) as _;
    pub const TARGET0_LO: *mut u32        = (BASE + 0x20) as _;
    pub const COMP0_LOAD: *mut u32        = (BASE + 0x50) as _;
    pub const REAL_TARGET0_LO: *const u32 = (BASE + 0x74) as _;
    pub const REAL_TARGET0_HI: *const u32 = (BASE + 0x78) as _;
    pub const INT_ENA: *mut u32           = (BASE + 0x64) as _;
    pub const INT_CLR: *mut u32           = (BASE + 0x6C) as _;

    pub const PCR_SYSTIMER_CONF: *mut u32      = 0x6009_6054 as _;
    pub const PCR_SYSTIMER_FUNC_CLK: *mut u32  = 0x6009_6058 as _;
}

/// 确保 SYSTIMER 外设时钟已启用 (APB clock + function clock)
///
/// 必须在任何 SYSTIMER 寄存器访问之前调用!
/// WiFi blob 的 `set_rx_gain_table` 等函数可能通过 modem 电源管理
/// 关闭 SYSTIMER 时钟，导致后续 SYSTIMER 访问触发 load access fault。
///
/// 返回 true 表示时钟被修复了。
#[inline(always)]
pub(crate) unsafe fn ensure_systimer_clock() -> bool {
    let mut fixed = false;

    let conf = core::ptr::read_volatile(regs::PCR_SYSTIMER_CONF);
    if conf & 0x1 == 0 || conf & 0x2 != 0 {
        core::ptr::write_volatile(regs::PCR_SYSTIMER_CONF, (conf | 0x1) & !0x2);
        fixed = true;
    }

    let func_clk = core::ptr::read_volatile(regs::PCR_SYSTIMER_FUNC_CLK);
    if func_clk & (1u32 << 22) == 0 {
        core::ptr::write_volatile(regs::PCR_SYSTIMER_FUNC_CLK, func_clk | (1u32 << 22));
        fixed = true;
    }

    fixed
}

/// Systick ISR handler — 清除中断并重载下一次 alarm
///
/// 返回值: 被跳过的 tick 数 (0 = 正常, N = 跳过了 N 个周期)
#[inline(always)]
pub(crate) unsafe fn systick_isr() -> u32 {
    use regs::*;

    ensure_systimer_clock();

    core::ptr::write_volatile(INT_CLR, 0x1);

    let conf = core::ptr::read_volatile(CONF);
    core::ptr::write_volatile(CONF, conf & !(1u32 << 24));

    let lo = core::ptr::read_volatile(REAL_TARGET0_LO);
    let hi = core::ptr::read_volatile(REAL_TARGET0_HI);
    let (mut next_lo, carry) = lo.overflowing_add(SYSTIMER_PERIOD);
    let mut next_hi = if carry { (hi + 1) & 0xFFFFF } else { hi };

    core::ptr::write_volatile(UNIT0_OP, 1u32 << 30);
    while core::ptr::read_volatile(UNIT0_OP as *const u32) & (1 << 29) == 0 {}
    let cur_lo = core::ptr::read_volatile(UNIT0_VALUE_LO);
    let cur_hi = core::ptr::read_volatile(UNIT0_VALUE_HI);

    let cur_val: u64 = ((cur_hi as u64) << 32) | (cur_lo as u64);
    let mut missed: u32 = 0;
    loop {
        let next_val: u64 = ((next_hi as u64) << 32) | (next_lo as u64);
        if next_val > cur_val {
            break;
        }
        let (nl, c) = next_lo.overflowing_add(SYSTIMER_PERIOD);
        next_lo = nl;
        if c { next_hi = (next_hi + 1) & 0xFFFFF; }
        missed += 1;
        if missed > 100 { break; }
    }

    core::ptr::write_volatile(TARGET0_LO, next_lo);
    core::ptr::write_volatile(TARGET0_HI, next_hi);
    core::ptr::write_volatile(COMP0_LOAD, 1);

    let conf = core::ptr::read_volatile(CONF);
    core::ptr::write_volatile(CONF, conf | (1u32 << 24));

    core::ptr::write_volatile(INT_ENA, core::ptr::read_volatile(INT_ENA) | 0x1);

    missed
}

/// 恢复 WiFi blob 可能破坏的外设时钟和指令缓存
///
/// 参数 mepc: 出错指令的地址 (用于判断是否在 flash 范围)
/// 返回 true 表示尝试了修复。
pub(crate) unsafe fn recover_peripheral_clocks(mepc: u32) -> bool {
    let mut repaired = false;

    // 1. SYSTIMER 时钟
    repaired |= ensure_systimer_clock();

    // 2. MSPI 时钟
    const PCR_MSPI_CONF: *mut u32 = 0x6009_6018 as *mut u32;
    let mspi = core::ptr::read_volatile(PCR_MSPI_CONF);
    if mspi & 0x5 != 0x5 || mspi & 0x2 != 0 {
        core::ptr::write_volatile(PCR_MSPI_CONF, (mspi | 0x5) & !0x2);
        repaired = true;
    }

    // 3. Cache 时钟
    const PCR_CACHE_CONF: *mut u32 = 0x6009_6104 as *mut u32;
    let cache = core::ptr::read_volatile(PCR_CACHE_CONF);
    if cache & 0x1 == 0 || cache & 0x2 != 0 {
        core::ptr::write_volatile(PCR_CACHE_CONF, (cache | 0x1) & !0x2);
        repaired = true;
    }

    // 4. 指令缓存脏数据修复
    static mut LAST_FENCED_MEPC: u32 = 0;
    let in_flash = mepc >= 0x4200_0000 && mepc < 0x4400_0000;

    if repaired {
        core::arch::asm!("fence.i", options(nomem, nostack));
        core::ptr::write_volatile(&raw mut LAST_FENCED_MEPC, mepc);
        return true;
    }

    if in_flash && core::ptr::read_volatile(&raw const LAST_FENCED_MEPC) != mepc {
        core::arch::asm!("fence.i", options(nomem, nostack));
        core::ptr::write_volatile(&raw mut LAST_FENCED_MEPC, mepc);
        return true;
    }

    false
}
