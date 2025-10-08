# CLAUDE.md - Development Guidelines

## Project Philosophy

Qontinui Runner is a desktop application for executing Qontinui automation projects. Prioritize clean, maintainable code and good user experience.

## Key Guidelines

### Code Quality Priority

- Write clean, maintainable code
- Keep implementations simple and straightforward
- Follow Rust and React best practices
- Ensure proper error handling and logging

### Architecture

- Tauri framework (Rust backend + React frontend)
- Python bridge for automation execution
- HAL (Hardware Abstraction Layer) for cross-platform support

## Git Commit Messages

**Joshua Spinak is the sole contributor to this project.**

- DO NOT add "Co-Authored-By: Claude" or similar lines
- DO NOT add "Generated with Claude Code" or similar attribution
- Keep commit messages professional and focused on the changes
- Use conventional commit format (e.g., "feat:", "fix:", "docs:", "refactor:")
