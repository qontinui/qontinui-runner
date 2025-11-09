# Qontinui Runner v0.1.0

Initial release of the Qontinui Runner desktop application.

## About

Qontinui Runner is a desktop application for executing [Qontinui](https://github.com/qontinui/qontinui) GUI automation configurations locally. Built with Tauri (Rust) + React (TypeScript).

## Features

- ✅ Execute automation configurations locally
- ✅ Real-time execution monitoring
- ✅ Mock and real execution modes
- ✅ Load and manage JSON configurations
- ✅ Cross-platform support (Windows, macOS, Linux)

## Installation

### Windows

Download and run the MSI installer:
- [Qontinui Runner_0.1.0_x64_en-US.msi](https://github.com/qontinui/qontinui-runner/releases/download/v0.1.0/Qontinui.Runner_0.1.0_x64_en-US.msi)

**⚠️ Windows SmartScreen Warning:** You'll see a "Windows protected your PC" warning because the installer isn't code-signed (code signing certificates cost $$$). This is normal for open-source projects. To install:
1. Click "More info"
2. Click "Run anyway"

If concerned about security, verify the SHA256 checksum or [build from source](https://github.com/qontinui/qontinui-runner#readme).

### macOS / Linux

Build from source (see [README](https://github.com/qontinui/qontinui-runner#readme))

## Requirements

- Python 3.10+ with qontinui and multistate installed
- See [README](https://github.com/qontinui/qontinui-runner#readme) for full setup instructions

## Links

- **Documentation:** [github.com/qontinui/qontinui](https://github.com/qontinui/qontinui)
- **MultiState Docs:** [qontinui.github.io/multistate](https://qontinui.github.io/multistate/)
- **Research Paper:** [Springer SoSyM](https://link.springer.com/article/10.1007/s10270-025-01319-9)
- **Visual Builder:** [qontinui.com](https://qontinui.com) (Early Access)

## Notes

- This is an early release (v0.1.0)
- Windows installer available now
- macOS/Linux builds coming soon if there's demand
- Report issues: [GitHub Issues](https://github.com/qontinui/qontinui-runner/issues)

## Verification

SHA256 checksum available in `checksums.txt` attachment.
