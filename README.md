# Qontinui Runner

![Version](https://img.shields.io/badge/version-0.1.0-blue)
![License](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)
![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)

Desktop application for running [Qontinui](https://github.com/qontinui/qontinui) GUI automation projects.

Built with Tauri (Rust) + React (TypeScript) for a native, performant desktop experience.

## Features

- Execute automation configurations locally
- Real-time execution monitoring
- Load and manage JSON configurations
- Cross-platform support (Windows, macOS, Linux)

### Agentic Features

Qontinui Runner includes advanced agentic capabilities for robust AI-assisted automation:

| Feature                                                           | Description                                                   |
| ----------------------------------------------------------------- | ------------------------------------------------------------- |
| [Memory Compression](docs/features/memory-compression.md)         | Prevents context overflow by compressing historical data      |
| [Retry with Feedback](docs/features/retry-feedback.md)            | Recovers from transient failures with error context injection |
| [Task Routing](docs/features/task-routing.md)                     | Routes tasks to appropriate AI models by complexity           |
| [Context Propagation](docs/features/context-propagation.md)       | Passes data between execution steps with expression syntax    |
| [Lifecycle Hooks](docs/features/lifecycle-hooks.md)               | Triggers custom actions at execution milestones               |
| [Accessibility Explorer](docs/features/accessibility-explorer.md) | Captures and interacts with accessibility trees               |

See [Agentic Features Overview](docs/features/agentic-features.md) for details.

### Developer Tools

Advanced features for workflow design, debugging, and optimization:

| Feature                                          | Description                                                    |
| ------------------------------------------------ | -------------------------------------------------------------- |
| [Learning Dashboard](docs/LEARNING_DASHBOARD.md) | Track AI learning patterns, strategy performance, and insights |
| [Flow Designer](docs/FLOW_DESIGNER.md)           | Visual editor for deterministic, step-by-step workflows        |
| [Checkpoint Browser](docs/CHECKPOINT_BROWSER.md) | Time-travel debugging with replay functionality                |

See [Features Overview](docs/FEATURES.md) for details and [API Reference](docs/API_REFERENCE.md) for complete API documentation.

### AWAS Integration

AWAS (AI Web Action Standard) enables AI-driven web automation through standardized action manifests:

| Feature                                       | Description                                                                     |
| --------------------------------------------- | ------------------------------------------------------------------------------- |
| [AWAS Builder](docs/features/awas-builder.md) | Discover, configure, and test AWAS actions                                      |
| AWAS Steps                                    | Five step types for workflows (discover, execute, check support, list, extract) |
| Manifest Discovery                            | Automatic detection of `/.well-known/ai-actions.json`                           |
| Action Execution                              | Execute AWAS actions with typed parameters                                      |

**Benefits over vision-based automation:**

- 10-100x faster execution
- No visual template maintenance
- Structured input/output validation
- Clear action documentation

See the [AWAS Builder Guide](docs/features/awas-builder.md) for usage details.

## Installation

### Download Pre-built Binaries

**Latest Release: [v0.1.0](https://github.com/qontinui/qontinui-runner/releases/tag/v0.1.0)** (Pre-release)

#### Windows

Download and run the MSI installer:

- **[Qontinui Runner v0.1.0 (MSI)](https://github.com/qontinui/qontinui-runner/releases/download/v0.1.0/Qontinui.Runner_0.1.0_x64_en-US.msi)** _(Recommended)_
- **[Qontinui Runner v0.1.0 (EXE)](https://github.com/qontinui/qontinui-runner/releases/download/v0.1.0/Qontinui.Runner_0.1.0_x64-setup.exe)** _(Alternative)_

**⚠️ Windows SmartScreen Warning:** You'll see a "Windows protected your PC" warning because the installer isn't code-signed. This is normal for open-source projects. To install:

1. Click "More info"
2. Click "Run anyway"

For security verification, check the [SHA256 checksums](https://github.com/qontinui/qontinui-runner/releases/tag/v0.1.0).

**Requirements:** Python 3.10+ with qontinui and multistate installed (see Prerequisites below).

#### macOS / Linux

Pre-built binaries coming soon. For now, build from source (see instructions below).

---

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

**For testing and configuration validation**, use [qontinui-web](https://qontinui.com)'s mock execution mode (launching Feb 2026), which simulates automation logic in your browser without requiring a GUI environment.

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

## Wrappers

The wrapper subsystem (install, list, dispatch, credentials) is **owned by the primary runner**.
Secondary runners (those launched with `QONTINUI_INSTANCE_NAME` set, typically by the supervisor)
do not bootstrap a local wrapper registry — their `/wrappers/*` HTTP routes proxy through the
primary at `http://127.0.0.1:$QONTINUI_PRIMARY_PORT`. This means installing a wrapper once on the
primary makes it visible to every runner, and the install-time filesystem watcher only runs in
one place.

If a secondary cannot reach the primary, `/wrappers/*` returns
`503 { error: "primary runner is not reachable; install wrappers from the primary's UI" }`.

### Wrapper MCP server (Claude CLI)

The runner ships a small stdio MCP server (`wrappers_mcp`) that exposes every
installed wrapper action as a tool to a [Claude CLI](https://docs.anthropic.com/en/docs/agents/claude-code/) session.
Tool names follow `wrapper_<wrapperId>__<actionId>` (dashes in the action id are
replaced with underscores). Each tool call is forwarded to the runner's
`POST /wrappers/<id>/dispatch` endpoint over HTTP on `127.0.0.1`.

```bash
# Build
cargo build --release --bin wrappers_mcp

# Default — primary on localhost:9876
claude mcp add qontinui-wrappers -- "$(pwd)/src-tauri/target/release/wrappers_mcp"

# Non-default port (matches QONTINUI_PRIMARY_PORT used elsewhere in the runner)
QONTINUI_PRIMARY_PORT=9878 \
  claude mcp add qontinui-wrappers -- "$(pwd)/src-tauri/target/release/wrappers_mcp"

# Override base URL (loopback only — http://(127.0.0.1|localhost):<port>)
QONTINUI_RUNNER_PRIMARY_URL=http://localhost:9878 \
  claude mcp add qontinui-wrappers -- "$(pwd)/src-tauri/target/release/wrappers_mcp"
```

After registration, every `claude` session has access to the wrapper actions
as tools (e.g. `wrapper_v0__create_component`). The MCP server connects to
the runner on demand at startup; if the runner is not running, the server
starts with an empty tool list and logs the failure to stderr.

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

See [qontinui documentation](https://github.com/qontinui/qontinui) for details.

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

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

Please note that this project is released with a [Code of Conduct](CODE_OF_CONDUCT.md). By participating in this project you agree to abide by its terms.

## License

Licensed under the GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later). See [LICENSE](LICENSE) for full terms.

## Related Projects

- **[qontinui](https://github.com/qontinui/qontinui)** - Core automation library (Python)
- **[multistate](https://github.com/qontinui/multistate)** - State machine library | [Docs](https://qontinui.github.io/multistate/)
- **[qontinui-web](https://qontinui.com)** - Web-based visual builder (launching Feb 2026)
- **[Brobot](https://github.com/jspinak/brobot)** - Original Java implementation

## Research

Based on [Model-based GUI Automation](https://link.springer.com/article/10.1007/s10270-025-01319-9) published in Springer SoSyM (October 2025).

## Built With

- [Tauri](https://tauri.app/) - Desktop app framework
- [React](https://reactjs.org/) - UI framework
- [Rust](https://www.rust-lang.org/) - Backend
- [TypeScript](https://www.typescriptlang.org/) - Frontend
- [Qontinui](https://github.com/qontinui/qontinui) - Automation engine (Python)
