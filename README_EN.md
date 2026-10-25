# RustTerm Egui

**A pure-Rust terminal client** (egui prototype; goal: replace the main project's Tauri + xterm.js frontend).

Stack: **egui 0.36 (eframe/glow rendering) + alacritty_terminal 0.26 (VT state machine) + vendored russh (SSH) + portable-pty (local PTY) + tokio + wry 0.47 (WebView2 embedded browser) + ironrdp / vnc-rs / capsaicin (remote desktop)**.

Lives outside the main repo (`rustterm-egui`) — no files in the main project are touched.
It only shares `vendor/russh`, and the SSH backend mirrors the main project's `src-tauri/src/ssh.rs` API usage
(connect / auth / channel-select event loop), keeping the same legacy-algorithm compatibility (SHA-1/CBC/des),
so Jumpserver / older devices work.

> 中文版：[README.md](README.md)

---

## Features

### Terminal & Sessions
- **Local terminal** (portable-pty + ConPTY) and **SSH terminal** (password / key auth, with key file picker & passphrase)
- **ProxyJump**: real `direct-tcpip` forwarding, jump-host handle kept alive; credentials follow the target host's auth method
- **Telnet** (raw TCP + minimal IAC negotiation), **Ping** (runs `ping.exe -t` in a PTY)
- **Session management**: `sessions.json` persistence, create/edit/delete, recent sessions, quick-connect bar
  (address | jump host | auth method | credentials | connect), and a "🔗 Connect" button in the New Session dialog that saves + connects in one click
- **Batch execution** (send a command to all terminals), **synchronized input** (mirror active terminal keystrokes to others),
  **port forwarding tunnel** (local TCP listener → target, start/stop, connection count), **find** (Ctrl+F match highlight + jump), **macros** (encoded control-byte streams written to the active terminal)
- Connect from the sidebar via double-click / context menu; unified tab bar (icon + title + close + drag reorder + double-click rename)

### SFTP
- Panel binds to the **active SSH session**, reusing the authenticated connection — no re-handshake, no re-auth (works through jump hosts)
- Remote directory browsing; upload/download (64KB chunks + progress + cancel); rename/mkdir/delete; hidden-file filtering
- **Drag-and-drop upload**: drop a file onto the panel to upload; the whole panel highlights while dragging
- The list **auto-refreshes** after upload/delete/mkdir/rename completes; the hidden-files toggle re-renders without re-listing

### Remote Desktop (VNC / RDP / SPICE)
- All three protocols genuinely implemented behind one interface (`RdCommand` input / `RdEvent` frames) with RGBA8 dirty-rect incremental rendering
- VNC security negotiation runs a two-pass handshake (with credentials, then fallback without); RDP goes through CredSSP; raw errors are translated into actionable hints by `friendly_error()`
- Key mapping: scan codes + X11 keysyms with `printable_keysym` shift handling for uppercase/symbols; modifier diffs are synthesized from events; unmapped keys fall back to a Unicode channel
- `fit`/`zoom` scaling, keyboard capture, pointer/wheel forwarding; auto-connect when opened from a saved session

### Embedded Browser
- wry / WebView2 native child window, one WebView per tab; navigation/title callbacks keep the address bar in sync
- Bookmarks / history (shared BrowserService), back/forward/reload
- Sits side-by-side with the AI panel (the WebView area's right edge is clamped to the panel's left edge)

### AI Assistant
- Multiple providers: OpenAI-compatible (covers relays/GLM/Kilo), local Ollama, Anthropic; settings persist to disk
- **Automatic terminal context**: each send prepends a terminal-info header (SSH user@host / Telnet / local) plus the last N terminal lines; the user bubble shows the exact payload that was sent
- **Attachments**: 📷 screenshots (GDI client-area capture), 📎 files (text inlined with 64KB truncation; PNG/JPEG images go through the vision channel), plus window drag-and-drop
- Agent toggle, read-only mode gating, command extraction with "send to terminal", bounded chat history

### General
- **Chinese/English switching** (~90-entry static table; unknown keys return as-is) and **dark/light themes** applied live
- Every password field has an 👁 eye toggle (state kept in egui Memory)
- All dialogs centered; window size adapts to the work area (`SPI_GETWORKAREA` + DPI)
- Config/session persistence falls back next to the exe (`data\`) when the primary path fails, with a status-bar toast

---

## Build / Run

```powershell
cd rustterm-egui
cargo run --release   # recommended; debug works too (deps compiled with opt-level=2)
cargo test            # sftp_client path utilities + telnet IAC unit tests
```

- On the dev machine, compile checks go through `check-msvc.bat` (wraps vcvars64 + INCLUDE)
- `russh` runs with default-features off (avoids aws-lc-rs); `debug=0` keeps binaries small

## Architecture

```
src/
  main.rs        eframe entry (glow backend) + window auto-fit
  app.rs         shell: menu bar + toolbar + quick connect + tab strip + central panel + status bar
  term.rs        core: Term<TermPane::PtyProxy> state machine, per-cell painting (Painter),
                 keyboard/mouse → VT encoding, selection copy, scrollback, find highlight, IME preedit
  tabs.rs        unified tab model: Connecting / Terminal / Browser / RemoteDesktop / LocalFiles / Failed
  sessions.rs    session model + sessions.json persistence + fallback path
  config.rs      AppConfig (with AiSettings; serde defaults migrate old configs)
  fonts.rs       system font injection (Consolas primary + Microsoft YaHei CJK fallback)
  backends/
    ssh.rs       russh: password/key auth, ProxyJump (direct-tcpip), SshHandle.open_sftp()
    local.rs     portable-pty local shell
    tcp.rs       raw TCP (Telnet, minimal IAC negotiation)
  sftp.rs        right-docked SFTP panel (binds active SSH session + drag-drop upload)
  sftp_client.rs SftpSession wrapper: list/transfer/cancel/progress + path utilities
  remote/        VNC (vnc-rs vendor) / RDP (ironrdp) / SPICE (capsaicin) behind one interface
  rdp.rs         remote-desktop tab rendering (ColorImage + incremental TextureHandle updates)
  browser.rs     wry WebView child windows (lazy creation, visibility sync, edge clamping)
  ai.rs          multi-provider request building/response parsing + command extraction & danger checks
  ai_assistant.rs chat panel (bubbles / attachments / terminal context)
  ai_config_panel.rs AI settings panel (save → config.save())
  screenshot.rs  GDI screen capture (BitBlt → PNG → base64)
  tunnel.rs      local TCP forwarder
  filebrowser.rs single-column local file browser (main tab)
  macros.rs      key macros (key combos → control-byte streams)
  i18n.rs        Chinese/English static dictionaries
  widgets.rs     shared widgets (password_field eye toggle, etc.)
```

**Data flow**: `pty/ssh → mpsc → vte Processor.advance(Term) → renderable_content() → egui Painter`;
input: `egui events → VT sequences → mpsc → pty/ssh` (terminal device-query replies loop back via `PtyProxy`).

---

## Recent changes (from the dev log)

| Date | Notes |
|---|---|
| 10-24 | Seven rounds of fixes (dialogs/forms/browser/macros/theme); XShell-style UI rework (session sidebar + welcome page + status bar); real embedded browser (wry child window); unified tabs + multi-provider AI; quick-connect password/key auth + window-size fix; real ProxyJump + three remote-desktop protocols + SFTP sidebar; SFTP rewritten as a real remote browser; language switching, menu adjustments, keyboard-capture fixes, SFTP auto-popup |
| 10-25 (rc1) | Remote-desktop function keys fixed; session/AI settings saving fixed; browser covering the AI panel fixed (right-edge clamp); AI chat auto-attaches terminal info header + recent output |
| 10-25 (rc2) | AI file/image attachments with drag-and-drop; SFTP drag-and-drop upload (drop point fixed via real cursor coordinates); auto-refresh after SFTP upload/delete/mkdir/rename |

## Development notes (egui / wry pitfalls)

1. `egui::Panel` claims parent space in `show()` order; `CentralPanel` must be shown last;
2. Hand-drawn widgets need `Sense::click()` (`clicked()` is always false with `Sense::hover`);
3. `wry::Rect` uses `LogicalPosition`; `SetWindowPos` on a **child window** takes parent-client coordinates — don't add screen offsets;
4. `load_html` produces `data:html` navigations — filter them in the nav callback;
5. The WebView2 child window always paints above egui: hide the WebView while any overlay is open (`overlay_open()`);
6. WebView2 steals system focus on creation — call `SetFocus(main window)`;
7. Never bind inputs to temporary variables; store them in struct fields and don't overwrite while focused;
8. Multiple `ScrollArea`s in one Ui need distinct `.id_salt()`;
9. During OLE drag-and-drop egui's pointer position is stale (winit ignores DragOver position) — compute the drop point via `GetCursorPos` + `ScreenToClient`;
10. egui 0.36's `CentralPanel` may not fully yield next to a resizable right panel: clamp native child-window areas to the same-frame panel rect.

## Known gaps

- SSH host-key verification: `check_server_key` accepts unconditionally (port the main project's `hostkey.rs` policy for release)
- VNC does not sync CapsLock yet; drag-and-drop upload is files-only (directory upload needs recursion in `sftp_client`)
- AI attachments are text / PNG-JPEG only; other binaries are rejected
- Performance: per-cell `painter.text` re-layouts every frame; could batch per-line or move to a wgpu vertex cache
- Dynamic palette: 256 colors use the static xterm table; `term.colors()` / main-project theming not wired

## Relationship with the main project

- No code sharing, except `vendor/russh` and the API usage patterns from `src-tauri/src/ssh.rs`
- Once this prototype's validations pass (rendering performance / CJK / keyboard-mouse / SFTP / remote desktop / browser / AI),
  the `term.rs` paint and input-encoding layers can move into the main project to replace the xterm.js frontend
