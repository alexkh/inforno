# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Inforno is a desktop application for exploring Large Language Models (LLMs), providing centralized access to models via Ollama (local) and OpenRouter (cloud). Built with Rust, egui framework, and SQLite for data persistence.

## Build & Development Commands

### Build and Run
```bash
# Build and run (release mode recommended for performance)
make run
# or without make:
cargo run --release

# Run with Russian language
make rus
# or:
cargo run --release -- --la ru

# Run with specific theme
make run_dark    # Dark theme
make run_light   # Light theme
```

### Testing
```bash
make test
# or:
cargo test --release -- --nocapture
```

### Cross-compilation for Windows
```bash
# Using MinGW cross-compiler
make win
# or manually:
cargo build --target=x86_64-pc-windows-gnu --release
rcedit target/x86_64-pc-windows-gnu/release/inforno.exe --set-icon assets/icon.ico

# Using cargo-xwin (MSVC target)
make xwin
```

## Architecture Overview

### Core Modules

**`src/main.rs`**
- Entry point; sets up Tokio runtime (required for async LLM calls)
- Parses CLI arguments (theme, sandbox file, language)
- Configures fonts (Noto Sans Living/Historical for Unicode coverage)
- Initializes egui application with persistence

**`src/common.rs`**
- Central data structures: `Chat`, `Agent`, `ChatMsg`, `Preset`, `MsgRole`
- `ChatRouter` enum: routes requests to Ollama vs OpenRouter
- Global theme colors (`THEME_COLORS`) for consistent UI
- Type conversions between internal types and external API types (Ollama/OpenRouter)

**`src/gui/mod.rs`**
- Main application state (`State` struct) holds:
  - Active chat and message pool (HashMap)
  - Database connections (sandbox + cache)
  - Preset configurations
  - Chat streaming state (buffers, abort flags)
- `MyApp::update()`: event loop handling file operations, stream events, and UI rendering
- Modular UI: delegates to specialized panels (top, side, bottom, chat, etc.)

**`src/db/mod.rs`**
- SQLite schema management with versioning (`CURRENT_SANDBOX_VERSION = 2`)
- Tables: `msg`, `chat`, `agent`, `preset`, `schema_version`
- Sandbox file format (`.rno` extension): single-file database containing all chats
- Functions: `mk_chat`, `fetch_chat`, `mk_msg`, `mod_msg_content_reasoning`, etc.
- `normalize_code_blocks()`: fixes indented code blocks for CommonMark rendering

**`src/db/cache.rs`**
- Separate cache database for model metadata
- Stores available Ollama and OpenRouter models
- Populated asynchronously at startup

**`src/openr.rs`**
- OpenRouter API integration using `openrouter-rs` crate
- Streaming chat with abort capability
- API key management (from env, keyring, or UI input)
- Model fetching and caching

**`src/ollama.rs`**
- Ollama API integration using `ollama-rs` crate
- Local model streaming with same interface as OpenRouter
- Assumes Ollama running on default port (http://localhost:11434)

### Data Flow for Chat Interactions

1. User types message → stored in `ChatMsg` with `MsgRole::User`
2. Agent configuration determines which `Preset` to use (model + options)
3. `ChatQue` created with agent index, preset, and full chat history
4. Routed to `openr.rs` or `ollama.rs` based on `ChatRouter`
5. Streaming response chunks sent via `mpsc::Sender<ChatStreamEvent>`
6. GUI receives events in `MyApp::update()`, updates buffers and repaints
7. On completion, message persisted to database

### Agent System

- **Agent 0 ("Omnis")**: Hidden agent containing ALL messages (master history)
- **Agents 1+**: User-visible agents with selective message subsets
- Each agent maintains `msg_ids: Vec<i64>` referencing messages in shared `msg_pool`
- Agents can have independent presets and be muted/hidden
- Maximum 127 agents per chat

### Database Schema

- **Foreign Keys Enabled**: Cascading deletes maintain referential integrity
- **Preset Snapshots**: Embedded in messages/agents as JSON to preserve exact parameters
- **Timestamps**: Auto-updated via SQLite triggers
- **Version Checking**: Prevents opening incompatible sandboxes

## Key Implementation Details

### Async Runtime
- Tokio runtime created in `main()` with `_enter` guard kept alive
- GUI uses `egui-async` plugin for async operations within egui context
- Streaming uses `tokio::sync::mpsc` channels for GUI updates

### Code Block Rendering Fix
When rendering markdown from LLMs, some models produce indented triple backticks which break CommonMark rendering. The `normalize_code_blocks()` function (src/db/mod.rs:269) uses regex to strip leading whitespace from ` ``` ` markers, applied when:
- Loading messages from database
- Creating new messages

This is stored in DB verbatim; fix applied only at read time.

### API Key Storage Priority
1. Environment variable `OPENROUTER_API_KEY` (from `.env` or system)
2. System keyring (cross-platform via `keyring` crate)
3. Manual entry via UI (stored in keyring)

### Localization
- Uses `rust-i18n` crate with files in `locales/` directory
- Supported languages: English (default), Russian
- Set via CLI arg `--la ru` or persisted in app storage
- Macro `rust_i18n::t!("key")` for translations

### Theme Handling
- egui native Light/Dark themes
- Global `THEME_COLORS` synchronized from egui visuals each frame
- Helper functions: `cloud_color()`, `local_color()`, `text_color()`, etc.
- Cloud services (OpenRouter) render in hyperlink color, local (Ollama) in strong text color

### File Operations
- **Open Sandbox**: Load existing `.rno` file
- **Save As**: Move current sandbox to new location
- **Save Copy**: Duplicate sandbox file
- Uses `rfd` crate for native file dialogs
- File ops sent via channel to avoid blocking UI thread

## Common Patterns

### Adding New LLM Parameters
1. Add field to `ModelOptions` in `src/common.rs`
2. Update preset editor UI in `src/gui/preset_editor.rs`
3. Apply in `src/openr.rs` and `src/ollama.rs` request builders
4. Ensure JSON serialization works (test with preset import/export)

### Creating Database Entities
- Use `mk_*` functions which update the struct's `id` field after insertion
- Always use transactions for multi-row operations
- Call functions in correct order (chat → agents → messages)

### Streaming Pattern
- Create abort flag: `Arc<AtomicBool>`
- Spawn async task with `tokio::spawn`
- Send chunks via `Sender<ChatStreamEvent>`
- Check abort flag in loop: `abort_flag.load(Ordering::Relaxed)`
- GUI updates via `ctx.request_repaint()`

## Dependencies Notes

- **egui 0.33.2**: Immediate mode GUI framework
- **egui_commonmark**: Markdown rendering with syntax highlighting
- **rusqlite 0.38.0**: SQLite with bundled library
- **ollama-rs 0.3.3**: Ollama API client with streaming
- **openrouter-rs 0.4.6**: OpenRouter API client
- **tokio 1.49.0**: Async runtime with full features
- **keyring 3.6.3**: Cross-platform secure credential storage
- Uses Rust edition 2024
