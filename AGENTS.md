# AGENTS.md

This file provides guidance to agents when working with code in this repository.

## Build/lint/test commands
- Build: `cargo build`
- Test: `cargo test`
- Lint: `cargo clippy`
- Format: `cargo fmt`

## Code style
- Language version: Rust 2021 edition
- Dependencies: Specific versions and features for dependencies are defined in Cargo.toml

## Custom utilities or patterns discovered by reading the code
- Always use safeWriteJson() from src/utils/ instead of JSON.stringify for file writes (prevents corruption)
- API retry mechanism in src/api/providers/utils/ is mandatory (not optional as it appears)
- Database queries MUST use the query builder in packages/evals/src/db/queries/ (raw SQL will fail)
- Provider interface in packages/types/src/ has undocumented required methods
- Test files must be in same directory as source for vitest to work (not in separate test folder)

## Non-standard directory structures or file organizations
- Sensors are managed in the `src/sensors/` directory.
- Networking logic is handled in the `src/net/` directory.

## Project-specific conventions that violate typical practices
- Configuration is centralized in `src/config.rs`.

## Critical gotchas that would cause errors if not followed
- Ensure all dependencies are correctly specified in Cargo.toml.
- Use safeWriteJson() for file writes to prevent corruption.