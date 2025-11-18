# Project Debug Rules (Non-Obvious Only)

## Hidden log locations not mentioned in docs
- Webview dev tools accessed via Command Palette > "Developer: Open Webview Developer Tools" (not F12)
- IPC messages fail silently if not wrapped in try/catch in packages/ipc/src/
- Production builds require NODE_ENV=production or certain features break without error
- Database migrations must run from packages/evals/ directory, not root
- Extension logs only visible in "Extension Host" output channel, not Debug Console

## Non-standard debugging tools or flags
- None specific to this project

## Gotchas that cause silent failures
- None specific to this project

## Required environment variables for debugging
- NODE_ENV=production for production builds