# Project Documentation Rules (Non-Obvious Only)

## Hidden or misnamed documentation
- None specific to this project

## Counterintuitive code organization
- src/ contains ESP32 embedded Rust code, not typical application code
- Sensors are managed in the `src/sensors/` directory with ADC and MH-Z19 CO2 sensor modules
- Dynamic indication system in `src/dynamic_indication/` handles Nixie tube control

## Misleading folder names or structures
- None specific to this project

## Important context not evident from file structure
- Uses `#![no_std]` and `#![no_main]` for bare-metal embedded development
- HTML files stored in `src/net/html/` for embedded web interface