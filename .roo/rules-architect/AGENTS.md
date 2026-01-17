# Project Architecture Rules (Non-Obvious Only)

## Hidden coupling between components
- Nixie tube control requires precise timing with timer interrupts configured properly
- Hardware-specific GPIO pin assignments are tightly coupled to physical hardware layout

## Undocumented architectural decisions
- Uses defmt for logging with DEFMT_LOG="info" environment variable
- Network functionality includes both WiFi and web server capabilities

## Non-standard patterns that must be followed
- Uses `#![no_std]` and `#![no_main]` for bare-metal embedded development
- mk_static! macro creates StaticCell instances for global data storage in embedded environment

## Performance bottlenecks discovered through investigation
- Memory allocation is limited; heap size is set to 72KB in main binary file
- Hardware timer interrupt handler is defined externally at the bottom of main file