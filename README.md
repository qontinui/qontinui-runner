# Qontinui Runner

![Version](https://img.shields.io/badge/version-0.1.0-blue)
![License](https://img.shields.io/badge/License-MIT-yellow.svg)
![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)

Desktop application for running [Qontinui](https://github.com/yourusername/qontinui) GUI automation projects.

## Features

- 🚀 Execute automation configurations locally
- 📊 Real-time execution monitoring
- 🔄 Mock and real execution modes
- 💾 Load and manage JSON configurations
- 🖥️ Cross-platform support (Windows, macOS, Linux)

## Installation

### Prerequisites

- **Python 3.10+** with qontinui and multistate installed
- **Node.js 18+** and npm (for building from source)
- **Rust** (for building from source)

### Quick Start

```bash
# Install dependencies
cd multistate && poetry install && cd ..
cd qontinui && poetry install && cd ..
cd qontinui-runner && npm install

# Run in development mode
npm run tauri dev
```

### Platform-Specific Setup

#### Windows

```bash
# Install Rust
winget install Rustlang.Rustup

# Install Python libraries
cd multistate && poetry install && cd ..
cd qontinui && poetry install && cd ..

# Run the app
cd qontinui-runner
npm install
npm run tauri dev
```

**Note**: WSL cannot perform GUI automation as it's headless. Use native Windows.

#### macOS

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Python libraries
cd multistate && poetry install && cd ..
cd qontinui && poetry install && cd ..

# Run the app
cd qontinui-runner
npm install
npm run tauri dev
```

#### Linux

```bash
# Install system dependencies
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Python libraries
cd multistate && poetry install && cd ..
cd qontinui && poetry install && cd ..

# Run the app
cd qontinui-runner
npm install
npm run tauri dev
```

## Usage

1. **Start the application**

   ```bash
   npm run tauri dev
   ```

2. **Start Python Executor**
   - Click "Start Executor" button
   - Choose execution mode (Mock or Real)

3. **Load Configuration**
   - Click "Load Config"
   - Select your automation JSON file

4. **Execute**
   - Click "Start" to run your automation
   - Monitor progress in real-time

## Execution Mode

**Qontinui Runner performs REAL GUI automation only.**

- ✅ Executes actual mouse clicks, keyboard input, and screen interactions
- ✅ Performs real image recognition using OpenCV template matching
- ✅ Requires active display (not headless/SSH environments)
- ✅ Suitable for production automation workflows
- ✅ Multi-monitor support for targeting specific displays

**For testing and configuration validation**, use [qontinui-web](https://github.com/jspinak/qontinui-web)'s mock execution mode, which simulates automation logic in your browser without requiring a GUI environment.

## Project Structure

```
qontinui-runner/
├── src/                      # React frontend (TypeScript)
│   ├── components/           # UI components
│   ├── services/             # API services
│   └── App.tsx              # Main app
├── src-tauri/               # Tauri backend (Rust)
│   ├── src/                 # Rust code
│   └── Cargo.toml           # Rust dependencies
├── python-bridge/           # Python → qontinui bridge
│   └── qontinui_bridge.py  # Minimal bridge script
└── public/                  # Static assets
```

## Building for Production

```bash
# Build for current platform
npm run tauri build

# Output locations:
# Windows: src-tauri/target/release/bundle/msi/
# macOS:   src-tauri/target/release/bundle/dmg/
# Linux:   src-tauri/target/release/bundle/appimage/
```

## Configuration Format

Qontinui Runner uses JSON configurations created by qontinui-web or written manually:

```json
{
  "version": "1.0",
  "states": [...],
  "processes": [...],
  "images": [...]
}
```

See [qontinui documentation](https://github.com/yourusername/qontinui) for details.

## Troubleshooting

### Windows

**"cargo: command not found"**

- Close and reopen PowerShell after installing Rust
- Or manually add to PATH: `C:\Users\YourUsername\.cargo\bin`

**Antivirus blocking build**

- Add exclusion for `.cargo` directory
- Temporarily disable real-time protection during first build

### macOS

**"xcrun: error"**

- Install Xcode Command Line Tools: `xcode-select --install`

### Linux

**"webkit2gtk not found"**

- Install dependencies: `sudo apt install libwebkit2gtk-4.1-dev`

**GUI automation not working**

- Ensure you're running on a display (not SSH/headless)
- Check permissions for input control

## Contributing

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

MIT License - See [LICENSE](LICENSE) file for details.

## Related Projects

- **[qontinui](https://github.com/yourusername/qontinui)** - Core automation library
- **[qontinui-web](https://github.com/yourusername/qontinui-web)** - Web-based configuration editor
- **[multistate](https://github.com/jspinak/multistate)** - Multi-state state management

## Built With

- [Tauri](https://tauri.app/) - Desktop app framework
- [React](https://reactjs.org/) - UI framework
- [Rust](https://www.rust-lang.org/) - Backend
- [TypeScript](https://www.typescriptlang.org/) - Frontend
- [Qontinui](https://github.com/yourusername/qontinui) - Automation engine
