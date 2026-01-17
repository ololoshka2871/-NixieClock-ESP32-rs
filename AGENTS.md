# AGENTS.md

This file provides guidance to agents when working with code in this repository.

## Build/lint/test commands
- Build: `cargo build`
- Test: `cargo test`
- Lint: `cargo clippy`
- Format: `cargo fmt`
- Flash to device: `cargo run` (uses espflash runner defined in .cargo/config.toml)

## Code style
- Language version: Rust 2021 edition
- Target: xtensa-esp32-none-elf for ESP32 compilation
- Uses `#![no_std]` and `#![no_main]` for bare-metal embedded development
- Requires `esp-alloc` heap allocator with size: 72 * 1024 bytes
- Uses defmt for logging with DEFMT_LOG="info" environment variable

## Custom utilities or patterns discovered by reading the code
- mk_static! macro creates StaticCell instances for global data storage in embedded environment
- Hardware-specific GPIO pin assignments are defined in src/bin/nixieclock-esp32-rs.rs for Nixie tube control
- Voltage calculation uses specific resistor divider ratios defined in src/config.rs
- Nixie clock timer operates at 100 microseconds period as defined in src/config.rs

## Non-standard directory structures or file organizations
- Sensors are managed in the `src/sensors/` directory with ADC and MH-Z19 CO2 sensor modules
- Dynamic indication system in `src/dynamic_indication/` handles Nixie tube control
- Network functionality in `src/net/` includes both WiFi and web server capabilities
- HTML files stored in `src/net/html/` for embedded web interface

## Project-specific conventions that violate typical practices
- Configuration is centralized in `src/config.rs` with hardcoded WiFi credentials
- GPIO pins are hardwired to specific hardware functions as documented in README.md
- Hardware timer interrupt handler is defined externally at the bottom of main file

## Critical gotchas that would cause errors if not followed
- Ensure all dependencies are correctly specified in Cargo.toml with esp32-specific features
- ESP32 requires specific rustflags and build-std settings in .cargo/config.toml
- Hardware GPIO assignments must match physical connections described in README.md
- Nixie tube control requires precise timing with timer interrupts configured properly
- Memory allocation is limited; heap size is set to 72KB in main binary file