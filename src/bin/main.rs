#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use log::info;
use relay_board_five_chn::board;
use relay_board_five_chn::network;
use relay_board_five_chn::{api, relay};

extern crate alloc;

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

    let board: board::Board = board::init();

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98768);
    // COEX needs more RAM - so we've added some more
    esp_alloc::heap_allocator!(size: 64 * 1024);

    esp_rtos::start(board.rtos.rtos_timer, board.rtos.int0);

    // This task keeps exclusive ownership of the physical relay GPIO pins.
    // API and future board-safety tasks receive only the returned command handle.
    let relay_control = relay::start(board.relays, spawner);

    let stack = network::w5500::start(board.w5500, spawner).await;

    info!("Embassy initialized!");
    info!("Waiting for Ethernet link and DHCP configuration...");
    stack.wait_config_up().await;
    info!("Ethernet is configured");

    // Four workers allow four HTTP requests to be processed concurrently.
    for _ in 0..4 {
        spawner.spawn(api::http_server_task(stack, relay_control).unwrap());
    }

    loop {
        info!("Hello world!");
        Timer::after(Duration::from_secs(1)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.1.0/examples
}
