# Agent instructions

## Project
- `reditor` is a Rust terminal code and text editor built with Ratatui and Crossterm.
- Keep user-facing strings consistent with the existing Russian, English, German, and Spanish localization in `locales/`.
- Preserve the terminal application's keyboard and mouse interaction model.

## Repository map
- `src/app.rs`: editor state, documents, dialogs, key handling, file operations.
- `src/ui.rs` and `src/workspace_ui.rs`: Ratatui rendering and workbench UI.
- `src/studio.rs`: terminal, Cargo, LSP, Git and command palette integration.
- `src/i18n.rs`, `locales/`: translations.
- `plugins/`, `bundled/`, `runtime/`: plugins and helper runtimes.

## Working conventions
- Prefer existing app commands and dialogs when adding actions; avoid duplicating file-operation logic.
- Keep changes focused and preserve UTF-8, Unicode-width, and terminal restoration behavior.
- Do not execute project plugin source automatically. Plugins are loaded only from explicitly configured directories.
- Update `README.md` and relevant `docs/` pages when user-visible commands or shortcuts change.
- Use `cargo fmt` for Rust formatting and `cargo check` to verify Rust changes when requested or needed.
