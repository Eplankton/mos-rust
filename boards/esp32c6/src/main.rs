#![no_std]
#![no_main]
#![allow(unused)]
#![allow(static_mut_refs)]
#![allow(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod ble;
mod board_info;
pub(crate) mod bsp;
pub mod lcd;
mod print_rtt;
mod rtos_adapter;
pub mod shell;
pub mod tests;
mod trap;

use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    dma::{DmaRxBuf, DmaTxBuf},
    dma_buffers,
    gpio::{Level, Output, OutputConfig},
    spi::master::Spi,
    time::Rate,
};
use mipidsi::{
    Builder,
    interface::SpiInterface,
    models::ST7789,
    options::{ColorInversion, Orientation, Rotation},
};
use mos::kernel::{Scheduler, Task};

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    mos::kprintln!("PANIC: {}", info);
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}

esp_bootloader_esp_idf::esp_app_desc!();

#[allow(clippy::large_stack_frames)]
#[esp_hal::main]
fn main() -> ! {
    // 1. 初始化 RTT 并注册到 mos 打印层
    print_rtt::init();

    // 2. 初始化 esp-hal
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // 3. 堆
    esp_alloc::heap_allocator!(size: 256 * 1024);

    // ==================== LCD 初始化 ====================
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(32768);

    let spi = Spi::new(
        peripherals.SPI2,
        esp_hal::spi::master::Config::default().with_frequency(Rate::from_mhz(80)),
    )
    .unwrap()
    .with_sck(peripherals.GPIO7)
    .with_mosi(peripherals.GPIO6)
    .with_dma(peripherals.DMA_CH0)
    .with_buffers(
        DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap(),
        DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap(),
    );

    let di = SpiInterface::new(
        ExclusiveDevice::new(
            spi,
            Output::new(peripherals.GPIO14, Level::High, OutputConfig::default()),
            Delay::new(),
        )
        .unwrap(),
        Output::new(peripherals.GPIO15, Level::Low, OutputConfig::default()),
        alloc::vec![0u8; 4096].leak(),
    );

    let display = Builder::new(ST7789, di)
        .reset_pin(Output::new(
            peripherals.GPIO21,
            Level::Low,
            OutputConfig::default(),
        ))
        .display_size(206, 320)
        .orientation(Orientation::new().rotate(Rotation::Deg270))
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay::new())
        .unwrap();

    let backlight = Output::new(peripherals.GPIO22, Level::Low, OutputConfig::default());

    unsafe {
        lcd::store_display(display, backlight);
    }

    // ==================== RTOS 启动 ====================

    // 安装 trap handler
    unsafe {
        mos::arch::riscv::install_trap_handler();
    }

    // 初始化 SYSTIMER
    bsp::init_systimer(peripherals.SYSTIMER);

    // ==================== BLE 初始化 (两阶段) ====================
    let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
    ble::store_bt_peripherals(
        timg0.timer0,
        esp_hal::rng::Rng::new(peripherals.RNG),
        peripherals.BT,
    );

    let _ = Task::create_raw(
        ble::bt_init_task_entry,
        core::ptr::null_mut(),
        3,
        "bt_init",
        8192,
    );

    // 启动调度器 (永不返回)
    Scheduler::launch();
}
