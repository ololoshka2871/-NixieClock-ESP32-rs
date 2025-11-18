# Project Documentation Rules (Non-Obvious Only)

## Hidden or misnamed documentation
- None specific to this project

## Counterintuitive code organization
- "src/" contains VSCode extension code, not source for web apps (counterintuitive)
- Provider examples in src/api/providers/ are the canonical reference (docs are outdated)
- UI runs in VSCode webview with restrictions (no localStorage, limited APIs)

## Misleading folder names or structures
- None specific to this project

## Important context not evident from file structure
- Package.json scripts must be run from specific directories, not root
- Locales in root are for extension, webview-ui/src/i18n for UI (two separate systems)