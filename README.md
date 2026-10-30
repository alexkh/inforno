# Inforno
> An AI-Native Desktop GUI & IDE Environment for Exploring and Automating LLM Workflows in Rust.

![App Screenshot](doc/screenshot.png)

**Inforno** is an extensible desktop application built with [egui](https://github.com/emilk/egui) that provides centralized access to local and cloud-based Large Language Models (LLM). It combines interactive LLM chat sessions, Jupyter-style notebooks, local VFS sandboxing ("Realms"), embedded code editing, and background daemon automation.

---

## 📽 Intro Video

[![Inforno Intro Video](https://img.youtube.com/vi/oJyj0mroFtY/0.jpg)](https://www.youtube.com/watch?v=oJyj0mroFtY)

---

## 🏗 Workspace Architecture

The project is structured as a modular Cargo workspace:

| Crate | Type | Description |
| :--- | :--- | :--- |
| **`inforno_gui`** | `Binary` | The primary frontend application providing chat windows, notebook views, preset editors, and tile-based IDE layouts. |
| **`inforno_core`** | `Library` | Core backend engine handling SQLite storage, OpenRouter & Ollama client streaming, Realm path isolation, and Rhai scripting execution. |
| **`autorno`** | `Daemon` | (in development) Headless autonomous daemon worker that opens OS sockets to run automated prompt loops, scripts, and note cells across realms. |
| **`bulat`** | `Library / Binary` | Fast, lightweight text editor and side-by-side code diffing tool with custom themes and extensible syntax highlighting. |
| **`vaultorno`** | `Daemon` | A locker room for disposable secrets: keeps API keys per user and per app, without ever seeing them unencrypted. See [`vaultorno/README.md`](vaultorno/README.md). |
| **`vaultorno_client`** | `Library` | The API apps use to keep secrets in vaultorno. Encrypts inside the app; falls back to OS sealing when vaultorno isn't installed. |
| **`vaultorno_inspect`** | `Library` | vaultorno's inspector: which executable a process runs. |
| **`xtask`** | `Tool` | `cargo xtask build`: builds the release binaries and signs their vaultorno statements. |

---

## ✨ Key Features

* **Multi-Provider LLM Integration:**
  * **Ollama Support:** Connect to locally hosted models with automatic installed-model discovery and pull management.
  * **OpenRouter** Pay-as-you-go cloud access.
* **Realms & Secure VFS:**
  * Organize work across multiple projects (stable/unstable branches) within centralized "Realms."
  * Virtual File System (VFS) with alias-based path resolution (`/project_name/dev/src/main.rs`) preventing directory escaping and resolving canonical symlink paths securely.
* **Interactive Notebooks & Rhai Scripting:**
  * Jupyter-style Note Cells integrated into chats.
  * Execute Rhai scripts embedded directly inside Note Cells to automate iterative queries and prompt loops. (very experimental atm).
* **Embedded Editor & Merging (`bulat`):**
  * Custom syntax highlighting powered by `Logos` lexers and dynamic `Rhai` script loaders.
  * Interactive LLM patch application and side-by-side diff merging directly inside chat panes.
* **Typst & Math Rendering:**
  * Embedded Typst compiler for inline and block LaTeX/math SVG rendering inside assistant responses.

---

## 🔑 Key & Credentials Setup

Chats and local state are stored in `.rno` SQLite database sandboxes.

To use OpenRouter models, click the **API Key** button in the top menubar:

* **Set temporarily** keeps the key in memory until Inforno closes.
* **Save in vault** keeps it for 90 days, encrypted: in [vaultorno](vaultorno/README.md) when it's installed, otherwise sealed for your user by the OS (`systemd-creds` on Linux, DPAPI on Windows). When the 90 days are up, Inforno asks for the key again, a good moment to issue a fresh one.

On openrouter.ai, create keys that expire within 90 days and have a spend limit. Deleting Inforno's copy doesn't stop a key someone else has already copied; its expiration at OpenRouter does.

Inforno never reads API keys from environment variables or `.env` files, and sends a saved key only to the hosts saved with it.

---

## 🛠 Building & Running

### Prerequisites
* [Rust Compiler & Cargo](https://www.rust-lang.org/tools/install) (Edition 2024 / MSRV supported by workspace dependencies)
* C compiler toolchain for SQLite bundling (`cc` / `gcc` / `MSVC`)

### Running `inforno_gui`
From the workspace root:

```bash
# Run GUI application in development mode
cargo run

# Run GUI application in release mode
cargo run --release
```

With vaultorno installed, saving keys needs a signed build that you start directly:

```bash
cargo xtask build        # builds the release binaries and signs their statements
target/release/inforno
```

`cargo run` sets `LD_LIBRARY_PATH`, and vaultorno refuses programs started with it. Such a run still works, with keys set for the session only.

### Running Sub-Crates & Tools
```bash
# Run the Autorno daemon
cargo run -p autorno

# Run standalone Bulat editor/diff tool
cargo run -p bulat -- path/to/file.rs
cargo run -p bulat -- left.rs right.rs
```

### Cross-Compiling for Windows
```bash
# Build release executable for x86_64 Windows
cargo build --target=x86_64-pc-windows-gnu --release --bin inforno

# Set application icon (optional tool)
rcedit target/x86_64-pc-windows-gnu/release/inforno.exe --set-icon assets/icon.ico
```

---

## 📄 License

This project is licensed under MIT license.
