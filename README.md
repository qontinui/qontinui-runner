# Qontinui Runner

[![CI](https://github.com/your-username/qontinui-runner/actions/workflows/ci.yml/badge.svg)](https://github.com/your-username/qontinui-runner/actions/workflows/ci.yml)
[![Release](https://github.com/your-username/qontinui-runner/actions/workflows/release.yml/badge.svg)](https://github.com/your-username/qontinui-runner/actions/workflows/release.yml)
![Beta](https://img.shields.io/badge/status-beta-yellow)
![Version](https://img.shields.io/badge/version-0.1.0-blue)

Desktop automation executor for [Qontinui Web](https://qontinui.com). Run your automation projects locally with a secure, robust desktop application.

## 🚀 Features

### Core Functionality
- **Automation Execution**: Execute automation projects created in Qontinui Web
- **Real-time Monitoring**: Watch execution progress with detailed logging
- **Configuration Management**: Load and manage automation configurations
- **Multiple Execution Modes**: Support for process-based and state machine execution

### Production-Ready Features (Beta)
- ✅ **Comprehensive Error Handling**: Graceful error recovery with user-friendly messages
- ✅ **Panic Recovery**: Application continues running even after unexpected errors
- ✅ **Logging System**: Detailed logs saved locally for debugging
- ✅ **Auto-Updates**: Automatic updates to get the latest features and fixes
- ✅ **Status Indicators**: Real-time system status monitoring
- ✅ **Crash Reporting**: Automatic crash reports via Sentry (opt-in)

## 📦 Installation

### Download Pre-built Binaries

Download the latest release for your platform:

- **Windows**: [Download .msi installer](https://github.com/your-username/qontinui-runner/releases)
- **macOS**: [Download .dmg](https://github.com/your-username/qontinui-runner/releases)
  - Intel: `qontinui-runner_x64.dmg`
  - Apple Silicon: `qontinui-runner_aarch64.dmg`
- **Linux**: [Download .AppImage or .deb](https://github.com/your-username/qontinui-runner/releases)

### Build from Source

#### Prerequisites

- [Node.js](https://nodejs.org/) (v18 or later)
- [Rust](https://rustup.rs/) (latest stable)
- Platform-specific dependencies:
  - **Linux**: `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`
  - **macOS**: Xcode Command Line Tools
  - **Windows**: WebView2 (comes with Windows 10/11)

#### Build Steps

```bash
# Clone the repository
git clone https://github.com/your-username/qontinui-runner.git
cd qontinui-runner

# Install dependencies
npm install

# Development build with hot-reload
npm run tauri dev

# Production build
npm run tauri build
```

## 🎮 Usage

### Basic Usage

1. **Start the Runner**: Launch Qontinui Runner from your applications
2. **Start Python Executor**: Click "Start Executor" to initialize the automation engine
3. **Load Configuration**: Use "Load Config" to select your automation project file
4. **Execute**: Choose execution mode and start your automation

### Execution Modes

- **Mock Mode** (Default): Test your automations without actual execution
- **Real Mode**: Execute automations with actual system interactions

### Status Indicators

The application provides real-time status for:
- **Executor Status**: Running/Stopped state of the Python executor
- **Configuration**: Whether a valid configuration is loaded
- **Execution**: Current execution state
- **Connection**: Connection status to Qontinui Web (when available)

## 🔧 Configuration

### Environment Variables

- `RUST_LOG`: Set logging level (default: `info`)
- `SENTRY_DSN`: Sentry DSN for crash reporting (optional)

### Log Files

Logs are stored in:
- **Windows**: `%LOCALAPPDATA%\qontinui-runner\logs\`
- **macOS**: `~/Library/Application Support/qontinui-runner/logs/`
- **Linux**: `~/.local/share/qontinui-runner/logs/`

## 🛡️ Security

### Code Signing

- **Windows**: Signed with Authenticode certificate
- **macOS**: Signed and notarized with Apple Developer certificate
- **Linux**: AppImage includes signature

### Auto-Updates

Updates are delivered securely through GitHub releases with signature verification.

## 🐛 Beta Status

This is a **BETA** release. While the core functionality is stable, you may encounter bugs. Please report issues to help improve the application.

### Known Limitations

- Auto-update requires manual approval on first run
- Some antivirus software may flag the application (false positive)
- macOS users need to allow the app in Security & Privacy settings on first run

## 🤝 Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for details.

### Development Setup

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run tests: `npm run test`
5. Submit a pull request

## 📄 License

[MIT License](LICENSE) - See LICENSE file for details

## 🆘 Support

- **Documentation**: [docs.qontinui.com](https://docs.qontinui.com)
- **Issues**: [GitHub Issues](https://github.com/your-username/qontinui-runner/issues)
- **Community**: [Discord Server](https://discord.gg/qontinui)

## 🙏 Acknowledgments

Built with:
- [Tauri](https://tauri.app/) - Desktop application framework
- [React](https://reactjs.org/) - UI framework
- [Rust](https://www.rust-lang.org/) - Backend language
- [TypeScript](https://www.typescriptlang.org/) - Frontend language

## 📊 Telemetry

The application includes:
- **Crash Reporting**: Anonymous crash reports to improve stability (can be disabled)
- **Update Checks**: Periodic checks for new versions (can be disabled)

No personal data or automation content is collected.

---

**Note**: This is a beta release. Use in production at your own risk. We recommend testing thoroughly in your environment before deploying for critical workflows.