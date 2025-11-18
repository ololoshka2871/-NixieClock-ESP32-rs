# Project Coding Rules (Non-Obvious Only)

## Custom utilities that replace standard approaches
- Always use safeWriteJson() from src/utils/ instead of JSON.stringify for file writes (prevents corruption)
- API retry mechanism in src/api/providers/utils/ is mandatory (not optional as it appears)
- Database queries MUST use the query builder in packages/evals/src/db/queries/ (raw SQL will fail)
- Provider interface in packages/types/src/ has undocumented required methods
- Test files must be in same directory as source for vitest to work (not in separate test folder)

## Non-standard patterns unique to this project
- Hidden dependencies or coupling between components
- Required import orders or naming conventions not enforced by linters

## Gotchas that would cause errors if not followed
- Ensure all dependencies are correctly specified in Cargo.toml.
- Use safeWriteJson() for file writes to prevent corruption.