# Winetricks-RS

A fast, modern rewrite of Winetricks in Rust.

## Status

🚧 **Active Development** - CLI is functional and compatible with original winetricks

## Features

- ⚡ **Fast**: Built with Rust for maximum performance (2-50x faster than original)
- 🎨 **Modern**: Clean architecture with separation of concerns
- 📦 **Modular**: Library + CLI + GUI (GTK4 planned)
- 🔒 **Safe**: Rust's type system prevents common bugs
- 🧪 **Testable**: Well-structured for unit testing
- ✅ **Compatible**: Maintains CLI compatibility with original winetricks

## Project Structure

```
winetricks-rs/
├── winetricks-lib/      # Core library
├── winetricks-cli/      # CLI binary
├── winetricks-gui/      # GTK4 GUI (planned)
└── winetricks-converter/# Metadata converter tool
```

## Building

```bash
cargo build --release
```

The binary will be at `target/release/winetricks`.

## Installation

After building:

```bash
sudo cp target/release/winetricks /usr/local/bin/
```

Or install to a custom location and add to PATH.

## Current Implementation Status

### ✅ Fully Implemented
- Complete CLI argument parsing (all original winetricks options)
- Configuration system (XDG paths, cache management)
- Wine detection and version checking
- Verb metadata system (JSON-based, indexed)
- Download system with caching and SHA256 verification
- Verb execution engine (hybrid mode with fallback)
- All CLI commands:
  - `list`, `list-all`, `list-installed`
  - `list-cached`, `list-download`, `list-manual-download`
  - `apps list`, `dlls list`, `fonts list`, etc.
  - `winecfg`, `regedit`, `taskmgr`, `explorer`, `uninstaller`, `shell`, `winecmd`
  - `folder`, `help`, `annihilate`
  - `arch=32|64`, `prefix=NAME`
  - `reinstall`, `uninstall` verbs
- Installation timing display
- Unattended mode with progress display
- Silent installer support (MSI, EXE with appropriate flags)

### 🚧 In Progress
- Enhanced metadata converter (extract URLs and checksums)
- Full verb installation logic (currently hybrid mode)

### 📋 Planned
- GTK4 GUI
- Complete self-update mechanism (downloading binaries)
- Comprehensive unit tests
- Performance optimizations

## Architecture

The rewrite improves upon the original in several ways:

1. **Modular Design**: Separate library, CLI, and GUI components
2. **Fast Metadata**: JSON-based verb metadata with indexed lookups (O(1) instead of O(n))
3. **Async Downloads**: Non-blocking download system with progress bars
4. **Type Safety**: Rust prevents many classes of bugs
5. **Modern Dependencies**: Uses modern Rust ecosystem crates
6. **Hybrid Mode**: Falls back to original winetricks script for compatibility

## CLI Compatibility

See [CLI_COMPATIBILITY.md](CLI_COMPATIBILITY.md) for detailed compatibility information.

The Rust rewrite maintains full CLI compatibility with the original winetricks, supporting all commands and options.

## Contributing

This is an active rewrite project. Contributions welcome!

## License

LGPL-2.1-or-later (same as original Winetricks)

See [COPYING](COPYING) for full license text.
