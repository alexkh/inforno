# inforno
Desktop Application for Exploring Large Language Models

![App Screenshot](doc/screenshot.png)

The goal of the project is to create a desktop application for centralized
access to any or all of the LLM models supported by Ollama and Openrouter. For
simple tasks free models can do the job (they can even be run simultaneously).
For more complicated tasks it might be necessary to use the bigger, paid models.
Openrouter is prepayed pay-as-you-go service without any monthly or yearly fees.
Also, Openrouter allows you to limit each key's money usage so you cannot
overspend.

# Intro Video

[![Inforno Intro Video](https://img.youtube.com/vi/oJyj0mroFtY/0.jpg)](https://www.youtube.com/watch?v=oJyj0mroFtY)

The Chats are stored locally in Sandbox files with the extension of .rno

To use Openrouter, you need to provide an Openrouter key, which you can obtain
by simply logging in to openrouter.ai and pressing the "Get API Key" button.

Enter the key with the API Key button in the top menu bar:

- **Set temporarily** keeps it in memory until Inforno closes.
- **Save in vault** keeps it for 90 days, encrypted: in vaultorno when it's
  installed (see `vaultorno/README.md`), otherwise sealed for your user by
  the OS (`systemd-creds` on Linux, DPAPI on Windows). When the 90 days are
  up, Inforno asks for the key again, a good moment to issue a fresh one.

On openrouter.ai, create keys that expire within 90 days and have a spend
limit. Deleting Inforno's copy doesn't stop a key someone else has already
copied; its expiration at OpenRouter does.

Inforno doesn't read API keys from environment variables or `.env` files.

With vaultorno installed, saving keys needs a signed build
(`cargo xtask build`) that you start directly, not through `cargo run`.
cargo sets `LD_LIBRARY_PATH`, and vaultorno refuses programs started with
it.

You need Rust compiler and cargo to build this project. Makefile is optional,
it's just there to unify the devops across different build tools

# build and run:
make run

# or, if make is not installed:
cargo run --release

# to build for Windows:
make win

# or, without make:
cargo build --target=x86_64-pc-windows-gnu --release
rcedit target/x86_64-pc-windows-gnu/release/inforno.exe --set-icon assets/icon.ico


