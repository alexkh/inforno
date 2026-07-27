# Bulat: GUI Code Editor and Merge Tool

**Bulat** is a lightweight, responsive code editor and diff/merge GUI tool written in Rust. Built on top of the `egui` framework, it is designed to be highly embeddable within larger applications (such as LLM chat interfaces) while also functioning completely independently as a standalone desktop application.

## Table of Contents
1. [Features](#features)
2. [Command Line Usage](#command-line-usage)
3. [Architecture Overview](#architecture-overview)
4. [Embedding Bulat (Library Usage)](#embedding-bulat-library-usage)
5. [Syntax Highlighting & Theming](#syntax-highlighting--theming)
6. [LLM Integration Features](#llm-integration-features)

---

## Features

* **Dual Operation Modes**: Functions as either a single-file text editor or a side-by-side diff merge tool.
* **Side-by-Side Diffing**: Utilizes the `similar` crate with the Patience algorithm to present clear, synchronized file differences with interactive visual merge buttons.
* **Advanced Code Editor (`CodeEditor`)**:
  * Line numbering (with custom mapping for diff alignment).
  * In-editor search with synchronized scrolling and match highlighting.
  * Auto-complete powered by a Trie-based dictionary (pulls from syntax keywords and user-typed text).
* **Rich Syntax Highlighting**: 
  * Native tokenizers for Rust and C/C++ using the `logos` crate.
  * Regex-based parsing for Markdown, YAML, and text.
  * **Dynamic Plugins**: Load custom language syntaxes via Rhai scripting (`.rhai` files).
* **Built-in Theming**: Includes meticulously crafted palettes for Gruvbox (Dark/Light), GitHub (Dark/Light), Ayu (Dark/Mirage/Light), Sonokai, and a custom SV theme.

---

## Command Line Usage

When running Bulat as a standalone desktop application, its behavior changes based on the number of file arguments passed:

### 1. Editor Mode
Pass a single file to open Bulat as a standard code editor.
```bash
bulat path/to/file.rs
```

### 2. Diff / Merge Mode
Pass two files to open Bulat in a side-by-side diff layout. The left file is considered the "target" filesystem file, and the right file is the "new" or "modified" diff.
```bash
bulat path/to/original.rs path/to/modified.rs
```

### 3. Scratchpad Mode
Pass no arguments to open an empty, unsaved editor instance.
```bash
bulat
```

---

## Architecture Overview

* **`StandaloneBulat` (`src/main.rs`)**: The `eframe` application wrapper that handles CLI arguments and window rendering.
* **`DiffApp` (`src/lib.rs`)**: The core merge tool logic. It manages two `CodeEditor` instances, tracks horizontal/vertical scroll synchronization, pads missing lines with zero-width spaces (`\u{200B}`) for visual alignment, and manages the interactive merge buttons.
* **`BulatEngine` (`src/engine.rs`)**: A pure-logic module handling string manipulation, diff computation (`TextDiff::configure().algorithm(Algorithm::Patience)`), and diff application.
* **`CodeEditor` (`src/editor/mod.rs`)**: A highly customizable `egui` widget that acts as the text editor.
* **`Syntax` & `ColorTheme` (`src/editor/syntax/`, `src/editor/themes/`)**: Handles regex and Logos-based lexical analysis, mapping parsed `TokenType`s to `egui::Color32` values.

---

## Embedding Bulat (Library Usage)

Because Bulat is built on `egui`, integrating it into an existing Rust UI is straightforward.

### Using the Diff Merge Tool

To use the side-by-side merge tool:

```rust
use bulat::DiffApp;
use eframe::egui;

// Initialize the DiffApp with your left and right code strings
let mut diff_app = DiffApp::new(original_code, new_code);

// Inside your egui update loop:
egui::CentralPanel::default().show(ctx, |ui| {
    diff_app.show(ui);
});
```
*Note: `DiffApp::show()` automatically calculates row heights, updates underlying string states when edits occur, and re-computes diffs on the fly.*

### Using the Standalone Code Editor

To embed just the code editor in your app:

```rust
use bulat::editor::{CodeEditor, ColorTheme, Syntax};

// Inside your egui update loop:
CodeEditor::default()
    .id_source("my_custom_editor")
    .with_rows(20)
    .with_theme(ColorTheme::GRUVBOX)
    .with_syntax(Syntax::rust())
    .with_numlines(true)
    .show(ui, &mut my_code_string);
```

### Using the Auto-Completer

The editor supports a Trie-based auto-completer out of the box:

```rust
use bulat::editor::{CodeEditor, Completer};

let mut completer = Completer::new_with_syntax(&Syntax::rust()).with_user_words();

// Inside your egui update loop:
CodeEditor::default()
    .id_source("editor_with_autocomplete")
    .show_with_completer(ui, &mut my_code_string, &mut completer);
```

---

## Syntax Highlighting & Theming

### Theming
Bulat exports an array of `ColorTheme` presets. To change the theme, pass it into your `CodeEditor` builder:

```rust
// Available themes: GRUVBOX, GITHUB_DARK, AYU_MIRAGE, SONOKAI, SV, etc.
.with_theme(ColorTheme::SONOKAI) 
```

### Syntax Parsing
By default, Bulat includes fast `logos`-based parsers for **Rust** and **C/C++**, and regex-based parsers for **Markdown**, **YAML**, and **Rhai**.

To dynamically guess the syntax based on a file extension:
```rust
let syntax = Syntax::get_or_load(ctx, "rs"); // Returns Syntax::rust()
```

#### Dynamic Rhai Plugins
Bulat supports loading completely custom syntax definitions at runtime using **Rhai** scripts. The `Syntax::get_or_load` method will automatically attempt to load a `.rhai` syntax script from `~/.config/bulat/scripts/syntax/v1/` if a hardcoded parser isn't found. This enables extending the editor without recompiling the binary.

---

## LLM Integration Features

Because Bulat was extracted from a larger LLM-chat ecosystem (Inforno), it ships with specialized utilities for handling AI-generated code inside `src/engine.rs`:

* **`apply_llm_diffs(original: &str, snippet: &str) -> Option<String>`**: Parses the standard Git-conflict-style response format (`<<<<`, `====`, `>>>>`) commonly generated by LLMs and attempts to automatically splice the blocks into the original source code. Features aggressive fallback matching (fuzzy whitespace matching) to prevent brittle patch failures.
* **`find_function_spans(code: &str, fn_name: &str)`**: A lightweight semantic parser that extracts the start and end byte indices of a specific Rust function block by counting `{` and `}` intelligently (ignoring strings and comments). 
* **`try_splice_snippet(original: &str, snippet: &str)`**: Combines function span detection to automatically merge single-function updates generated by an LLM directly into the target file without requiring a full diff application.
