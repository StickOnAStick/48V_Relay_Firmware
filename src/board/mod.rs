/*
Board-level configurations. Pins & ESP HAL peri's initalization.

TODO: Integration with build.rs for dynamic allocation of pin configurations
*/

use esp_hal::{
    Async, clock::CpuClock, gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull}, interrupt::software::SoftwareInterrupt, peripherals::CPU_CTRL, spi::{Mode, master::{Config, Spi}}, time::Rate, timer::timg::{Timer, TimerGroup},
};
extern crate alloc;

pub const RELAY_COUNT: usize = 5;
pub struct Board {
    pub w5500: W5500Hardware,
    pub relays: RelayBank<RELAY_COUNT>,
    pub rtos: RtosResources,
}

pub struct W5500Hardware {
    pub spi: Spi<'static, Async>,
    pub cs: Output<'static>,
    pub int: Input<'static>,
    pub reset: Output<'static>,
}

impl W5500Hardware {
    pub fn new(
        spi: Spi<'static, Async>, 
        cs: Output<'static>,
        int: Input<'static>,
        reset: Output<'static>
    ) -> Self {
        Self { spi, cs, int, reset }
    }
}

pub struct RelayBank<const N: usize> {
    pins: [Output<'static>; N],
    active_high: bool,
}

impl <const N: usize> RelayBank<N> {
    pub fn new(pins: [Output<'static>; N], active_high: bool) -> Self {
        Self { pins, active_high }
    }

    pub fn count(&self) -> usize {
        N
    }

    pub fn set(&mut self, relay: usize, on: bool) -> Result<(), ()> {
        let pin = self.pins.get_mut(relay).ok_or(())?;
        let drive_high = on == self.active_high;

        if drive_high {
            pin.set_high();
        } else {
            pin.set_low();
        }

        Ok(())
    }

    pub fn all_on(&mut self) {
        for pin in &mut self.pins {
            if self.active_high {
                pin.set_high();
            } else {
                pin.set_low();
            }
        }
    }

    pub fn all_off(&mut self) {
        for pin in &mut self.pins {
            if self.active_high {
                pin.set_low();
            } else {
                pin.set_high();
            }
        }
    }
}

pub struct RtosResources { 
    // Do not use rtos_timer for generic timing, api timeouts, or arbitrary periodic work.
    pub rtos_timer: Timer<'static>,
    pub int0: SoftwareInterrupt<'static, 0>,
    pub int1: SoftwareInterrupt<'static, 1>,
    pub cpu_ctrl: CPU_CTRL<'static>,
}

pub fn init() -> Board {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let p = esp_hal::init(config);

    // The following pins are used to bootstrap the chip. They are available
    // for use, but check the datasheet of the module for more information on them.
    // - GPIO0 -- RESET
    // - GPIO2 -- NC
    // - GPIO5 -- NC
    // - GPIO12 -- JTAG TDI
    // - GPIO15 -- JTAG TDO
    // These GPIO pins are in use by some feature of the module and should not be used.
    let _ = p.GPIO6;
    let _ = p.GPIO7;
    let _ = p.GPIO8;
    let _ = p.GPIO9;
    let _ = p.GPIO10;
    let _ = p.GPIO11;
    // let _ = p.GPIO16; - Only an issue if you have an R variant wroom 32 with QSPI ram, in which you'll need to pull up this pin.
    let _ = p.GPIO20;

    let w5500 = W5500Hardware{
        spi: Spi::new(
            p.SPI2,  // Spi 0/1 are dedicated internal controllers.
            Config::default()
                .with_frequency(Rate::from_mhz(10))
                .with_mode(Mode::_0)
        )
        .unwrap()
        .with_sck(p.GPIO27)
        .with_mosi(p.GPIO25)
        .with_miso(p.GPIO26)
        .into_async(),

        cs: Output::new(
            p.GPIO33, 
            Level::High,
            OutputConfig::default(),
        ),

        int: Input::new(
            p.GPIO32,
            InputConfig::default().with_pull(Pull::Up),
        ),

        reset: Output::new(
            p.GPIO4,
            Level::High,
            OutputConfig::default()
        ),
    };

    let relays = RelayBank::new(
        [
            Output::new(p.GPIO16, Level::Low, OutputConfig::default()),
            Output::new(p.GPIO17, Level::Low, OutputConfig::default()),
            Output::new(p.GPIO18, Level::Low, OutputConfig::default()),
            Output::new(p.GPIO19, Level::Low, OutputConfig::default()),
            Output::new(p.GPIO23, Level::Low, OutputConfig::default()),
        ],
        true
    );

    let timg0 = TimerGroup::new(p.TIMG0);
    let sw_int = esp_hal::interrupt::software::SoftwareInterruptControl::new(p.SW_INTERRUPT);

    let rtos = RtosResources {
        rtos_timer: timg0.timer0,
        int0: sw_int.software_interrupt0,
        int1: sw_int.software_interrupt1,
        cpu_ctrl: p.CPU_CTRL,
    };

    Board { w5500, relays, rtos }
}
