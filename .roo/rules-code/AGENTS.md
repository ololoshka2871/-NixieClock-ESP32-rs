# Project Coding Rules (Non-Obvious Only)

## Custom utilities that replace standard approaches
- mk_static! macro creates StaticCell instances for global data storage in embedded environment
- Hardware-specific GPIO pin assignments are defined in src/bin/nixieclock-esp32-rs.rs for Nixie tube control
- Voltage calculation uses specific resistor divider ratios defined in src/config.rs

## Non-standard patterns unique to this project
- Uses `#![no_std]` and `#![no_main]` for bare-metal embedded development
- Hardware timer interrupt handler is defined externally at the bottom of main file
- Nixie clock timer operates at 100 microseconds period as defined in src/config.rs

## Gotchas that would cause errors if not followed
- ESP32 requires specific rustflags and build-std settings in .cargo/config.toml
- Memory allocation is limited; heap size is set to 72KB in main binary file
- Hardware GPIO assignments must match physical connections described in README.md