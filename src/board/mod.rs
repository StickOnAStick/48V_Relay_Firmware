/*
Board-level configurations. Pins & ESP HAL peri's initalization.
*/

use esp_hal::{
    Async,
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    spi::master::Spi,
};
extern crate alloc;

pub struct Board {
    pub w5500: W5500Hardware,
    pub relays: RelayHardware,
    pub scheduler: SchedulerHardware,
}

pub struct W5500Hardware {
    pub spi: i32,
    pub cs: i32,
    pub int: i32,
    pub reset: i32,
}

pub struct RelayHardware {
    
}

pub fn init() -> Board {
    
}
