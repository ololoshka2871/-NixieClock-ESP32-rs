# Project Architecture Rules (Non-Obvious Only)

## Hidden coupling between components
- Providers MUST be stateless - hidden caching layer assumes this

## Undocumented architectural decisions
- None specific to this project

## Non-standard patterns that must be followed
- Webview and extension communicate through specific IPC channel patterns only

## Performance bottlenecks discovered through investigation
- None specific to this project