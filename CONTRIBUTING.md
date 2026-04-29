<!-- BEGIN License & CLA preamble — added 2026-04-29 during AGPL rollout -->
## License & CLA

This project is licensed under the **GNU Affero General Public License v3.0 or later** (`AGPL-3.0-or-later`). See [`LICENSE`](LICENSE) for the full text. Contributors should be aware:

- AGPL is a strong copyleft license. Anyone who runs a modified version of this project as a network service must publish their modifications under AGPL too.
- For typical self-hosting, internal use, forking, or contributing back, AGPL behaves like GPL.

All non-trivial contributions require signing the qontinui Contributor License Agreement (CLA). The CLA is administered via [cla-assistant.io](https://cla-assistant.io/) — when you open a pull request, the CLA bot will comment with a one-click sign link, and signing applies across all qontinui repositories. The CLA text lives in [`CLA.md`](CLA.md). It grants Joshua Spinak the right to relicense your contribution under any future license; you retain copyright in your contributions.

The remainder of this document covers contribution mechanics specific to this repository.

<!-- END License & CLA preamble -->

# Contributing to Qontinui Runner

Thank you for your interest in contributing to Qontinui Runner! This document provides guidelines for contributing to the desktop application.

## Code of Conduct

Be respectful, constructive, and collaborative. We're all here to build something useful together.

## How to Contribute

### Reporting Bugs

1. Check if the bug has already been reported in [Issues](https://github.com/yourusername/qontinui-runner/issues)
2. If not, create a new issue with:
   - Clear title describing the problem
   - Steps to reproduce
   - Expected vs actual behavior
   - Operating system and version
   - Screenshots if applicable
   - Console logs from dev tools (if available)

### Suggesting Features

1. Check existing [Issues](https://github.com/yourusername/qontinui-runner/issues)
2. Create a new issue describing:
   - The problem you're trying to solve
   - Your proposed solution
   - Example use cases
   - UI mockups if applicable

### Pull Requests

1. **Fork the repository** and create a branch from `main`
2. **Install dependencies:**

   ```bash
   # Install Node dependencies
   npm install

   # Install Rust (if not already installed)
   # https://rustup.rs/

   # Install Python dependencies for bridge
   cd python-bridge
   pip install -r requirements.txt
   cd ..
   ```

3. **Make your changes:**
   - Frontend (React/TypeScript): Follow existing patterns
   - Backend (Tauri/Rust): Follow Rust best practices
   - Python bridge: Follow Python style guide
   - Write clear, documented code
   - Add tests for new functionality

4. **Test your changes:**

   ```bash
   # Run in development mode
   npm run tauri dev

   # Run frontend tests
   npm test

   # Build for production
   npm run tauri build
   ```

5. **Commit your changes:**
   - Use clear commit messages
   - Reference issues when applicable

6. **Push to your fork** and submit a pull request

7. **Address review feedback** if requested

## Development Setup

### Prerequisites

- **Node.js** 18+ and npm
- **Rust** 1.70+ (via rustup)
- **Python** 3.10+
- **Qontinui library** installed (`poetry install` in qontinui repo)
- **MultiState library** installed (`poetry install` in multistate repo)

### Setup Steps

```bash
# Clone your fork
git clone https://github.com/yourusername/qontinui-runner.git
cd qontinui-runner

# Install frontend dependencies
npm install

# Install Python bridge dependencies
cd python-bridge
pip install -r requirements.txt
cd ..

# Run in development mode
npm run tauri dev
```

## Project Structure

```
qontinui-runner/
├── src/                    # React frontend (TypeScript)
│   ├── components/         # React components
│   ├── services/           # API services
│   └── App.tsx            # Main app component
├── src-tauri/             # Tauri backend (Rust)
│   ├── src/               # Rust source code
│   └── Cargo.toml         # Rust dependencies
├── python-bridge/         # Python bridge to qontinui
│   └── qontinui_bridge.py # Bridge implementation
└── public/                # Static assets
```

## Code Style

### Frontend (TypeScript/React)

- Use TypeScript for type safety
- Follow React hooks patterns
- Use functional components
- Format with Prettier
- Lint with ESLint

### Backend (Rust)

- Follow Rust naming conventions
- Use `cargo fmt` for formatting
- Run `cargo clippy` for linting
- Handle errors properly (Result types)

### Python Bridge

- Follow PEP 8
- Use type hints
- Format with `ruff format`
- Minimal code - delegate to qontinui library

## Testing

### Frontend Tests

```bash
npm test
```

### End-to-End Testing

1. Build the app: `npm run tauri build`
2. Install and test the built application
3. Test on target platforms (Windows/Mac/Linux)

### Python Bridge Tests

```bash
cd python-bridge
pytest
```

## Building for Release

```bash
# Build for current platform
npm run tauri build

# Output will be in src-tauri/target/release/bundle/
```

## Platform-Specific Notes

### Windows

- Requires MSVC toolchain
- May need to exclude .cargo directory from antivirus

### macOS

- Requires Xcode command line tools
- App needs to be signed for distribution

### Linux

- Requires additional system dependencies (see Tauri docs)
- Different package formats available (AppImage, deb, rpm)

## Areas for Contribution

### Good First Issues

- UI improvements
- Bug fixes
- Documentation
- Example automations

### Feature Development

- New UI components
- Configuration editor improvements
- Execution monitoring
- Error reporting improvements

### Platform Support

- Linux support
- macOS optimization
- Mobile platforms (future)

## Architecture

Qontinui Runner is a **Tauri application** with three layers:

1. **Frontend** (React/TypeScript): User interface
2. **Backend** (Rust/Tauri): Native OS integration
3. **Python Bridge**: Communication with qontinui library

The Python bridge is minimal - it delegates all automation logic to the qontinui library.

## Dependencies

- **[Qontinui](https://github.com/yourusername/qontinui)** - Core automation library
- **[MultiState](https://github.com/jspinak/multistate)** - State management
- **Tauri** - Desktop app framework
- **React** - UI framework

## Questions?

- Open an issue for questions
- Check Tauri documentation for framework questions
- Check qontinui docs for automation questions

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

Thank you for contributing! 🎉
