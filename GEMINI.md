# Coop - Gemini Context

## Project Overview

Coop is a simple, fast, and reliable Nostr client for secure messaging across all platforms. It is written in Rust and structured as a Cargo workspace. The project utilizes:
- **GPUI**: The GPU-accelerated UI framework developed by Zed Industries for cross-platform, high-performance user interfaces.
- **Rust Nostr SDK**: For handling the Nostr protocol (including NIPs like 44, 49, 59, 96).

The workspace is divided into several sub-crates under `crates/`, including the main application (`coop`), UI components (`ui`, `chat_ui`, `title_bar`, `theme`), and domain logic (`chat`, `person`, `relay_auth`, `state`, `device`).

## Building and Running

### Prerequisites
- **Rust Toolchain**: Use `rustup` to install the Rust toolchain.
- **System Dependencies**: Depending on your OS, you must run the provided setup scripts to install necessary libraries before compiling.

### Commands
- **Install Linux Dependencies**:
  ```bash
  ./script/linux
  ```
- **Install FreeBSD Dependencies**:
  ```bash
  ./script/freebsd
  ```
- **Install macOS Dependencies**:
  ```bash
  ./script/macos
  ```
- **Build the project** (debug mode):
  ```bash
  cargo build
  ```
- **Run the application**:
  ```bash
  cargo run
  ```
- **Build for Production** (optimized release binary):
  ```bash
  cargo build --release
  ```

### Packaging
- Packaging scripts and manifests are available in the `script/` directory (e.g., `bundle-linux`, `bundle-snap`, `prepare-flathub`) and the `flathub/` directory.

## Development Conventions

- **Code Formatting**: The project enforces a strict Rust code formatting style via `rustfmt.toml`.
  - **Edition**: 2024
  - **Indentation**: Block style with 4 tab spaces.
  - **Imports**: Grouped by `StdExternalCrate`, with module granularity and automatic reordering of imports, modules, and impl items.
- **Modularity**: Code is split into focused crates (e.g., separating UI code in `chat_ui` from core messaging logic in `chat`). When contributing, ensure your code respects these boundaries.
- **Contributing**: Contributions are made via Pull Requests on the `lumehq/coop` GitHub repository. Ensure all tests pass before submitting.
- **UI Architecture**: Because Coop uses GPUI, UI components are built using GPUI's element tree structure, event handling, and view contexts.
