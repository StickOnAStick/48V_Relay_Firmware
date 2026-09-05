//! Relay ownership and commands shared by the API and board-safety code.
//!
//! The `relay_task` is the sole owner of the GPIO outputs. Other tasks use a
//! `RelayControl` handle, so no network-facing code can manipulate GPIO pins
//! directly.

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_executor::Spawner;
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    channel::Channel,
};

use crate::board::{RelayBank, RELAY_COUNT};

const COMMAND_QUEUE_DEPTH: usize = 8;

static COMMANDS: Channel<CriticalSectionRawMutex, RelayCommand, COMMAND_QUEUE_DEPTH> =
    Channel::new();
static RELAY_STATE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
enum RelayCommand {
    Set { index: usize, on: bool },
    AllOff,
}

/// A command handle that may be passed to API and safety tasks.
///
/// It deliberately contains no GPIO pins. GPIO ownership remains in
/// `relay_task`, which serializes every requested relay state change.
#[derive(Clone, Copy)]
pub struct RelayControl {
    _private: (),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayError {
    InvalidIndex,
}

impl RelayControl {
    /// Queue a change to one relay. Completion means the controller accepted
    /// the command; the relay task then applies it in FIFO order.
    pub async fn set(&self, index: usize, on: bool) -> Result<(), RelayError> {
        if index >= RELAY_COUNT {
            return Err(RelayError::InvalidIndex);
        }

        COMMANDS.send(RelayCommand::Set { index, on }).await;
        Ok(())
    }

    /// Queue a safe-state request for every relay.
    ///
    /// Board recovery, watchdog, or shutdown tasks can use this same handle;
    /// they do not need access to the physical GPIO outputs.
    pub async fn all_off(&self) {
        COMMANDS.send(RelayCommand::AllOff).await;
    }

    /// Returns a bit per relay: bit 0 is relay 0, bit 1 is relay 1, etc.
    pub fn state_mask(&self) -> u32 {
        RELAY_STATE.load(Ordering::Acquire)
    }
}

/// Start the one task that owns the relay GPIO pins and return the handle used
/// by API and board-safety code.
pub fn start(relays: RelayBank<RELAY_COUNT>, spawner: Spawner) -> RelayControl {
    spawner.spawn(relay_task(relays).unwrap());
    RelayControl { _private: () }
}

#[embassy_executor::task]
async fn relay_task(mut relays: RelayBank<RELAY_COUNT>) -> ! {
    // `board::init` creates every output low. Do it explicitly as well so the
    // runtime safe state is unambiguous before accepting any external command.
    relays.all_off();
    RELAY_STATE.store(0, Ordering::Release);

    loop {
        match COMMANDS.receive().await {
            RelayCommand::Set { index, on } => {
                // `RelayControl::set` validates the index. Keep this check in
                // case another future producer is added inside this module.
                if relays.set(index, on).is_ok() {
                    let bit = 1_u32 << index;
                    if on {
                        RELAY_STATE.fetch_or(bit, Ordering::AcqRel);
                    } else {
                        RELAY_STATE.fetch_and(!bit, Ordering::AcqRel);
                    }
                }
            }
            RelayCommand::AllOff => {
                relays.all_off();
                RELAY_STATE.store(0, Ordering::Release);
            }
        }
    }
}
