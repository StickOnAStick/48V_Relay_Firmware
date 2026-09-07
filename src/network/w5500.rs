//! W5500 Ethernet bring-up and the private background tasks it requires.

use embassy_executor::Spawner;
use embassy_net::{Config, Runner as NetRunner, Stack, StackResources};
use embassy_net_wiznet::{
    Device,
    Runner as WiznetRunner,
    State,
    chip::W5500,
};
use embassy_time::Delay;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    Async,
    efuse::{self, InterfaceMacAddress},
    gpio::{Input, Output},
    spi::master::Spi,
};
use static_cell::ConstStaticCell;

use crate::board::W5500Hardware;

/// Packet queues between the W5500 driver and the network stack.
///
/// Each entry costs approximately one Ethernet MTU of RAM. Four receive and
/// four transmit entries are a reasonable starting point for this controller.
// Construct the packet buffers in static storage, avoiding a large startup
// stack temporary when the async main task first runs.
static W5500_STATE: ConstStaticCell<State<4, 4>> =
    ConstStaticCell::new(State::new());

/// Four API sockets plus DHCP and one spare socket.
static NET_RESOURCES: ConstStaticCell<StackResources<6>> =
    ConstStaticCell::new(StackResources::new());

// A single W5500 is the exclusive user of this SPI bus. The wrapper supplies
// chip-select handling required by the W5500's `SpiDevice` interface.
type W5500SpiDevice = ExclusiveDevice<
    Spi<'static, Async>,
    Output<'static>,
    Delay,
>;

type W5500Runner = WiznetRunner<'static, W5500, W5500SpiDevice, Input<'static>, Output<'static>>;
type NetworkRunner = NetRunner<'static, Device<'static>>;


/*
Talks to PHY via SPI. PHY barrier.

Reacts to interupts, and processes incoming / outgoing requests. 
*/
#[embassy_executor::task]
async fn w5500_runner_task(runner: W5500Runner) -> ! {
    runner.run().await
}

/*
Runs TCP/IP Stack

ARP, DHCP, IP, TCP, timers, retransmissions, routing to/from TCP sockets.
*/
#[embassy_executor::task]
async fn net_runner_task(mut runner: NetworkRunner) -> ! {
    runner.run().await
}

/// Start Ethernet and return the handle used to create TCP/UDP sockets.
///
/// The W5500's SPI/GPIO resources are consumed here and remain private to the
/// driver runner. Application tasks receive only the returned `Stack`.
pub async fn start(
    hw: W5500Hardware,
    spawner: Spawner,
) -> Stack<'static> {
    let W5500Hardware {
        spi,
        cs,
        int,
        reset,
    } = hw;


    let spi_device = ExclusiveDevice::new(spi, cs, Delay).unwrap();

    // Use a locally administered address derived from the ESP32's factory MAC
    // so it does not collide with the station MAC if Wi-Fi is used later.
    let mut mac = [0; 6];
    mac.copy_from_slice(
        efuse::interface_mac_address(InterfaceMacAddress::AccessPoint).as_bytes(),
    );

    let (device, w5500_runner) = embassy_net_wiznet::new::<4, 4, W5500, _, _, _>(
        mac,
        W5500_STATE.take(),
        spi_device,
        int,
        reset,
    )
    .await
    .unwrap();

    spawner.spawn(w5500_runner_task(w5500_runner).unwrap());

    let seed = u64::from_be_bytes([0, 0, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]);
    let (stack, net_runner) = embassy_net::new(
        device,
        Config::dhcpv4(Default::default()),
        NET_RESOURCES.take(),
        seed,
    );

    spawner.spawn(net_runner_task(net_runner).unwrap());

    stack
}
