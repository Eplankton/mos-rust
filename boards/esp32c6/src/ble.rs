//! BLE Peripheral — 手机通过 GATT shell_cmd 特征发送 Shell 命令
//!
//! 两阶段初始化:
//!   1. main() 调用 store_bt_peripherals() 存储外设 (scheduler 启动前)
//!   2. bt_init_task → esp_wifi::init() → ble_task (scheduler 启动后)

use bleps::ad_structure::{
    AdStructure, BR_EDR_NOT_SUPPORTED, LE_GENERAL_DISCOVERABLE, create_advertising_data,
};
use bleps::attribute_server::{AttributeServer, WorkResult};
use bleps::{Ble, HciConnector, gatt};

use mos::kernel::Task;

// ============================================================
// 全局状态
// ============================================================

static mut TIMG0_TIMER: Option<esp_hal::timer::timg::Timer<'static>> = None;
static mut RNG: Option<esp_hal::rng::Rng> = None;
static mut ESP_WIFI_CTRL: Option<esp_wifi::EspWifiController<'static>> = None;
static mut BT_PERIPH: Option<esp_hal::peripherals::BT<'static>> = None;

/// 存储 BT 外设 — scheduler 启动前在 main() 中调用
pub fn store_bt_peripherals(
    timer: esp_hal::timer::timg::Timer<'static>,
    rng: esp_hal::rng::Rng,
    bt_periph: esp_hal::peripherals::BT<'static>,
) {
    mos::kprintln!("[ble] Storing peripherals for BT init task");
    unsafe {
        TIMG0_TIMER = Some(timer);
        RNG = Some(rng);
        BT_PERIPH = Some(bt_periph);
    }
}

/// BT 初始化任务入口
pub extern "C" fn bt_init_task_entry(_param: *mut core::ffi::c_void) {
    unsafe {
        core::arch::asm!("csrsi mstatus, 8");
    }

    mos::kprintln!("[bt_init] Running esp_wifi::init()...");

    let timer = unsafe { TIMG0_TIMER.take().expect("TIMG0 timer not stored") };
    let rng = unsafe { RNG.take().expect("RNG not stored") };

    let init = esp_wifi::init(timer, rng).unwrap();
    mos::kprintln!("[bt_init] esp_wifi::init() done");

    unsafe {
        ESP_WIFI_CTRL = Some(init);
    }

    let _ = Task::create_raw(ble_task_entry, core::ptr::null_mut(), 2, "ble", 8192);

    mos::kprintln!("[bt_init] BLE task created, done");
}

pub extern "C" fn ble_task_entry(_param: *mut core::ffi::c_void) {
    ble_task();
}

fn current_millis() -> u64 {
    mos::arch::riscv::yield_task();
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_millis()
}

fn cmd_write(_offset: usize, data: &[u8]) {
    crate::shell::shell_push_input(data);
}

fn ble_task() {
    mos::kprintln!("[ble] Task started");

    let ctrl_ref = unsafe { ESP_WIFI_CTRL.as_ref().expect("esp-wifi ctrl not set") };
    let bt_periph = unsafe { BT_PERIPH.take().expect("BT periph not set") };

    mos::kprintln!("[ble] Creating BleConnector...");
    let connector = esp_wifi::ble::controller::BleConnector::new(ctrl_ref, bt_periph);
    mos::kprintln!("[ble] BleConnector created");

    mos::kprintln!("[ble] Waiting for controller init...");
    Task::delay(500);

    let hci = HciConnector::new(connector, current_millis);
    let mut ble = Ble::new(&hci);

    mos::kprintln!("[ble] Initializing BLE stack...");
    ble.init().unwrap();
    ble.cmd_set_le_advertising_parameters().unwrap();

    ble.cmd_set_le_advertising_data(
        create_advertising_data(&[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteLocalName("MOS-ESP32C6"),
        ])
        .unwrap(),
    )
    .unwrap();

    ble.cmd_set_le_advertise_enable(true).unwrap();
    mos::kprintln!("[ble] Advertising started as 'MOS-ESP32C6'");

    // BLE 就绪，启动 LCD + Shell + Async 测试
    Task::create(crate::lcd::lcd_task, 1, "lcd").expect("Failed to create lcd task");
    Task::create(crate::shell::launch, 1, "shell").expect("Failed to create shell task");
    #[cfg(feature = "async")]
    crate::tests::async_test(10);
    mos::kprintln!("[ble] LCD + Shell + Async tasks created");

    let mut cmd_wf = cmd_write;

    gatt!([service {
        uuid: "937312e0-2354-11eb-9f10-fbc30a62cf38",
        characteristics: [characteristic {
            uuid: "937312e2-2354-11eb-9f10-fbc30a62cf38",
            name: "shell_cmd",
            write: cmd_wf,
        },],
    }]);

    let mut rng = bleps::no_rng::NoRng;
    let mut srv = AttributeServer::new(&mut ble, &mut gatt_attributes, &mut rng);

    let mut loop_count: u32 = 0;

    loop {
        match srv.do_work() {
            Ok(WorkResult::GotDisconnected) => {
                mos::kprintln!("[ble] Disconnected, re-advertising...");
            }
            Ok(WorkResult::DidWork) => {}
            Err(e) => {
                mos::kprintln!("[ble] Error: {:?}", e);
                Task::delay(100);
            }
        }

        loop_count = loop_count.wrapping_add(1);
        if loop_count % 1000 == 0 {
            mos::kprintln!("[ble] alive ({})", loop_count);
        }

        Task::delay(10);
    }
}
