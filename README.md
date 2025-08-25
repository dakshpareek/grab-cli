# CLI Downloader (Learning-by-Building)

This repository is **not** a polished production tool—it's a hands-on
playground for learning modern Rust while incrementally constructing a
feature-rich command-line download manager.

Example:

```bash
cargo run -- https://speed.hetzner.de/1MB.bin
```

## Getting Started

1. Install Rust:
   ```bash
   curl https://sh.rustup.rs -sSf | sh
   ```
2. Fetch dependencies & build:
   ```bash
   cd grab-cli
   cargo run -- --help
   ```
3. Download a file:
   ```bash
   cargo run -- https://example.com/file.zip
   ```

## TUI (Interactive)
- Run the TUI:
  ```bash
  cargo run --bin dlm_tui
  ```
- Keys: a (add URL), i (info), q (quit), ↑/↓ (select), Enter/Esc (modals)

## Why This Exists

> “Learn by doing.”
> Each commit introduces one new Rust concept, then applies it to expand
> the downloader’s capabilities.  The journey is the primary goal; the
> tool itself is the side-effect.
