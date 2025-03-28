# ESP32-C3 Template with debug

## Requirements
- Rust toolchain for riscv32i
  ```shell
  rustup target add riscv32imc-unknown-none-elf
  ```
- [espflash](https://github.com/esp-rs/espflash/tree/main/espflash) v3.2.0 Flash and run with `defmt`
  ```shell
  cargo install espflash
  ```
- [probe-rs](https://github.com/probe-rs/probe-rs) v0.24.0
  ```shell
  cargo install probe-rs
  ```

## Run with defmt
```shell
cargo run --release 
```

## Debug 
Using `probe-rs-debug`, see `.vscode\launch.json`.