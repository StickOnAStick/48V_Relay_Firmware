

enum RelayCommand {
    Set { relay: u8, on: bool }
}

static RELAY_COMMANDS: Chaannel<CriticalSectionRawMutex, RelayCommand, 8> = 
    Channel::new();