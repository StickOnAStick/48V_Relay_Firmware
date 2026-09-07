#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use esp_backtrace as _;
use esp_hal::system::Stack as CoreStack;
use esp_rtos::embassy::Executor;
use log::info;
use relay_board_five_chn::board;
use relay_board_five_chn::network;
use relay_board_five_chn::tasks::{api, relay};
use static_cell::{ConstStaticCell, StaticCell};

extern crate alloc;

// Core 1 needs its own CPU stack and Embassy executor. 8 KiB is a deliberate
// starting budget for the relay task and the RTOS core-1 thread; revise it
// after measuring real stack usage with the final firmware.
const SECOND_CORE_STACK_BYTES: usize = 8 * 1024;
// Initialize in static storage so startup never needs an 8 KiB stack temporary.
static SECOND_CORE_STACK: ConstStaticCell<CoreStack<SECOND_CORE_STACK_BYTES>> =
    ConstStaticCell::new(CoreStack::new());
static SECOND_CORE_EXECUTOR: StaticCell<Executor> = StaticCell::new();

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.3.0
    // generator parameters: --chip esp32 -o esp32-wroom-32e -o log -o esp-backtrace -o wokwi -o ci -o vscode -o esp -o stack-smashing-protection -o unstable-hal -o alloc -o wifi -o embassy -o ble-trouble

    esp_println::logger::init_logger_from_env();

    let board::Board {
        w5500,
        relays,
        rtos,
    } = board::init();

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98768);
    // COEX needs more RAM - so we've added some more
    esp_alloc::heap_allocator!(size: 64 * 1024);

    // Core 0 runs the Embassy main task, W5500 runner, TCP/IP stack, and API.
    esp_rtos::start(rtos.rtos_timer, rtos.int0);

    // Core 1 runs the GPIO-owning relay task. The `RelayControl` is only a
    // channel handle, so it may safely be copied to the core-0 API workers.
    let relay_control = relay::control();
    let second_core_stack = SECOND_CORE_STACK.take();
    esp_rtos::start_second_core(
        rtos.cpu_ctrl,
        rtos.int1,
        second_core_stack,
        move || {
            let executor = SECOND_CORE_EXECUTOR.init(Executor::new());
            executor.run(|core1_spawner| relay::start(relays, core1_spawner));
        },
    );

    let stack = network::w5500::start(w5500, spawner).await;

    info!("Embassy initialized!");
    info!("Waiting for Ethernet link and DHCP configuration...");
    stack.wait_config_up().await;
    info!("Ethernet is configured");
    if let Some(config) = stack.config_v4() {
        info!("Relay board: http://{}/ (gateway: {:?})", config.address.address(), config.gateway);
    }

    // Four workers allow four HTTP requests to be processed concurrently.
    for _ in 0..4 {
        spawner.spawn(api::http_server_task(stack, relay_control).unwrap());
    }

    // All application work is now performed by spawned tasks. Keep the main
    // task alive without consuming CPU or emitting generator-example logs.
    loop {
        core::future::pending::<()>().await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.1.0/examples
}
