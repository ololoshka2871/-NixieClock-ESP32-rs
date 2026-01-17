# Project Debug Rules (Non-Obvious Only)

## Hidden log locations not mentioned in docs
- Uses defmt for logging with DEFMT_LOG="info" environment variable
- ESP32 logs accessible via serial monitor when flashing with espflash

## Non-standard debugging tools or flags
- Flash to device using: `cargo run` which uses espflash runner defined in .cargo/config.toml

## Gotchas that cause silent failures
- Hardware timer interrupt handler is defined externally at the bottom of main file
- Memory allocation is limited; heap size is set to 72KB in main binary file

## Required environment variables for debugging
- DEFMT_LOG="info" for proper logging output