# mySSH

English | [简体中文](README.zh-CN.md)

[![CI](https://github.com/xinchen2003/mySSH/actions/workflows/ci.yml/badge.svg)](https://github.com/xinchen2003/mySSH/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-0.5.0-blue)](https://github.com/xinchen2003/mySSH/releases)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-lightgrey)](https://github.com/xinchen2003/mySSH)

**A local-first SSH client for Windows with a built-in MCP server — hand your servers to AI agents, on your terms.**

No login, no cloud dependency, credentials never leave your machine. Built with Tauri 2 (Rust + WebView2) and React.

## Why mySSH

- **AI-ready**: a built-in MCP server lets Claude Code / OMP / OpenCode run commands, read/write and transfer files on your saved sessions — with per-session × per-tool-group permissions you control
- **Local-first**: config in local SQLite, credentials in the Windows DPAPI vault, never on disk in plaintext
- **Performance budgeted**: terminal IPC streams over a binary Tauri Channel (not JSON events); tunnels / SFTP / monitoring each run on dedicated SSH connections, so bulk traffic never stutters your interactive shell

## Features

- **Terminal**: tabs, split panes in any direction, drag tabs out into separate windows, auto-reconnect; true color / wide Unicode / mouse reporting / bracketed paste / search / hyperlinks; optional input broadcasting across terminals (filterable by server); command snippets (insert from the palette, or type a trigger and press Tab to expand; multi-line snippets go in as one bracketed paste); in-terminal file transfer via ZMODEM (rz/sz) and trzsz; session recording (right-click → Start Recording; asciicast v2 files playable in the built-in player or with asciinema, exportable as plain text; keystrokes are not recorded)
- **Sessions**: nested group tree, move-to-group context menu, tags, favorites, fuzzy search, command palette (Ctrl+Shift+P); password / public-key (OpenSSH & .ppk) / keyboard-interactive (2FA) / agent auth; multi-hop ProxyJump; known_hosts confirmation on first connect and key change; login macros — run commands line-by-line automatically after reaching the shell; optional su second-login — sign in as a regular user and auto-switch to another account (e.g. root), password answered once at the prompt
- **Local sessions**: native Windows shells (PowerShell / pwsh / CMD) via ConPTY alongside SSH sessions — custom startup directory and startup command (e.g. launch an AI agent CLI on connect); shares tabs, splits, detached windows and input broadcasting
- **Tunnels**: local / remote / dynamic SOCKS5 forwarding, auto-start, auto-recover on disconnect; dedicated SSH connection isolated from interactive terminals
- **SFTP**: dual-pane file manager, bidirectional drag & drop (including OS files and folders), queued transfers (concurrency control, resume, retry), cross-session transfer history, remote file editing (auto-upload on save), follows the terminal's working directory (OSC 7)
- **Monitoring**: live CPU / memory / disk / network charts on an isolated channel, silently degrades on failure
- **Export**: plaintext or passphrase-encrypted (Argon2id + AES-256-GCM) config packages
- **Themes**: 15 built-in schemes (default mySSH Deep Space / Morning Mist, plus Tokyo Night / Catppuccin Mocha·Latte / Dracula / GitHub Dark / One Dark / Solarized / Nord / Midnight / GitHub / Eye-care Green / Warm) — the UI uses the system font (Segoe UI / Microsoft YaHei) while the terminal stays monospace, icons are consistent line SVGs, and the whole UI skin derives from the theme (surfaces, text, accent), not just the terminal palette; visual custom theme editor (no JSON hand-editing), custom terminal background image with adjustable opacity
- **i18n**: Simplified Chinese / English UI, switchable in Settings
- **Terminal encoding**: per-session remote encoding (UTF-8 / GBK / GB18030 / Big5 / Shift_JIS / EUC-KR), streaming transcode with zero-copy passthrough for UTF-8
- **Auto-update**: silently checks GitHub Releases on startup, downloads and installs on confirmation, then restarts; one-click diagnostics bundle (logs + system info as a zip) ready to attach to an issue

## AI Automation (MCP)

Built-in MCP server (Streamable HTTP, loopback-only + Bearer token); one-click agent config copy in Settings.

**18 tools**:

| Tool | Description |
| --- | --- |
| `list_sessions` | List saved session profiles (never includes credentials), with each session's effective AI access (level, production flag, which tool groups are allowed), open tab count, and the terminals the AI can use right away (including tabs you shared with the AI) |
| `ssh_exec` | Run a shell command on a session, returns stdout/stderr/exit code. Commands pass the built-in policy classifier at the session's access level; risky commands trigger an in-app approval popup (the call may return `pending_approval` — retry with the same arguments to keep waiting). Per-session rate-limited, all attempts audited. Output is token-friendly: ANSI sequences stripped, long output keeps head and tail. While the window is open, the command, its live output and exit code also appear in a read-only split pane below that session's terminal tab (the tab is opened automatically if needed; turn off via Settings → MCP → "AI command mirror") |
| `sftp_home` / `sftp_list` / `sftp_stat` / `sftp_read` | Remote browsing and file reads |
| `sftp_write` / `sftp_mkdir` / `sftp_delete` / `sftp_rename` / `sftp_chmod` | Remote writes and meta ops (audited) |
| `sftp_upload` / `sftp_download` | File transfer via the background transfer queue (shared with the UI transfer panel, **no size limit**; `wait_seconds` waits synchronously for completion) |
| `sftp_transfer_list` | Poll transfer progress / state / errors |
| `file_view` / `file_edit` / `file_grep` / `file_glob` | Remote files: paged view with line numbers, exact string-replace edits (unique match + etag guard against concurrent changes), regex search, glob find — all over SFTP, no remote command execution |
| `file_backups` / `file_rollback` | Automatic backup and rollback of AI changes: `file_edit` / `sftp_write` / `sftp_delete` back up the original first; rollback is refused by default if someone changed the file afterwards. Each record carries an optional AI-written `title` plus a computed change summary. Right-click a session → "AI File Changes…" for one-click rollback. Backups are kept 7 days and the audit log 30 days by default (Settings → General → History Retention) |
| `recipe_list` / `recipe_run` | Built-in ops recipes: 16 parameterized templates (system overview, disk usage, top processes, ports, service status/logs, Docker containers, service restart, signals…), each with a risk level (read / change / danger). Parameters are whitelist-validated before substitution; read recipes work even at the readonly level (e.g. `docker ps`), danger recipes always ask you first |
| `job_start` / `job_read` / `job_stop` / `job_list` | Background commands: start long-running commands (builds, `tail -f`, big downloads) and return immediately, page through output with a byte cursor (ANSI control sequences stripped and progress bars folded by default), stop any time; same command gate as `ssh_exec`, in the `ssh_exec` permission group |
| `terminal_open` / `terminal_send` / `terminal_read` / `terminal_close` / `terminal_list` | Interactive terminals: open a live shell on a session, send input, read the screen buffer, close it — for REPLs, pagers and other stateful interactions `ssh_exec` can't drive. `terminal_read` can wait server-side for a prompt regex (`wait_for`) or for output to go quiet (`idle_ms`), so the agent knows a command finished without polling. When the window is open the terminal appears as a normal tab: watch it, AI writes pause while you type, right-click to take back control; when the AI closes a terminal it opened, the tab closes too (a tab you shared stays open and you get a notice); you can also right-click an existing tab to "Share with AI" (own `terminal` permission group, **off by default**: raw keystrokes cannot pass the command policy). Every AI-controlled terminal is recorded automatically, including what the AI typed; when a recording ends its SHA-256 goes into the hash-chained audit log, so Command Palette → Session Recordings can verify a recording and the audit log were not modified afterwards |

**Permission model** (resolved session → nearest group → global):

1. Global defaults: Settings → MCP — 6 tool-group toggles (list_sessions / ssh_exec / SFTP read / SFTP write / SFTP transfer / terminal; terminal is off by default) and the access level (read-only by default / standard / full)
2. Group policies: right-click a group → MCP Policy — tool groups, access level and a production flag, inherited by subgroups and their sessions
3. Per-session overrides: session editor → MCP Permissions tab — every field can inherit or override. Lock down `ssh_exec` on production, keep test boxes wide open
4. Command policy (always on): `ssh_exec` commands are classified before execution. Read-only: whitelist only. Standard: anything not provably read-only needs approval. Full: only blacklisted commands need approval. Blacklist, indirect shells and unparseable structures always need approval, and so does every non-read-only command on a production session. The whitelist, blacklist and indirect-executor lists can be edited under Settings → Command policy, each with "Restore defaults"; changes apply to the next command without a restart (setting keys `policy.whitelist` / `policy.blacklist` / `policy.indirect`). Removing a wrapper such as `sudo` from the indirect list still checks the wrapped command, so `sudo rm` cannot bypass the blacklist. Approval is an in-app popup (deny is the default focus and shows the seconds left; auto-deny after 5 minutes). Simple command chains are summed up in one sentence (heredoc bodies are treated as data, file writes and `sudo` are called out); multi-line scripts with loops or conditions get one overall description (line count, commands used, whether files are written). The reasons point at the commands that actually run (e.g. "awk can run any command or write files"); keywords like `do`/`for` and `2>/dev/null` are not flagged. The optional `purpose` note the AI passed is shown (marked unverified). Checking "Auto-run (later commands on this session)" runs it after 3 s and turns on auto-run for that session: later commands from the same AI client on that session that need approval still show the popup, pre-checked with a 3 s countdown. High-risk (blacklisted) commands are excepted and always need a manual decision. Unchecking during a countdown turns it off; denying one command leaves it on; it ends when the AI client disconnects (AI control is released). The read-only whitelist covers common script commands such as `[`, `test`, `sleep`, `tr`, `cut`, `read`, `declare`, `local` (changing key variables like `PATH` or `LD_*` still needs approval). The popup prominently shows which session the command targets (color dot, name, user@host, group, red production marker) with a button that jumps to that session's terminal tab (opening one if needed). When several sessions have pending approvals, a left column groups them by session; you can "Deny all for this session", but approvals stay one at a time. The taskbar flashes while the window is in the background. The toolbar "AI" button opens an activity panel listing pending approvals, running commands, background jobs and AI terminals per session, each with a stop button; tabs and sidebar rows show "AI running n" / "n pending" badges. With no app window available it fails closed. Per-session rate limit via `mcp.rate_limit_per_minute` (default 60/min); every attempt and decision lands in the audit log
5. AI control ends when the client goes away: each AI client gets its own session id on connect and only its own leftovers are cleaned up. When a client closes (its long-lived stream drops and does not reconnect within 30 s, or it ends the session), mySSH denies and closes its pending approval popups, stops its running commands and background jobs, closes its AI terminals (UI tabs stay open, only AI control is taken back) and closes command mirrors no other client uses, then shows a notice listing what was cleaned. Tabs you shared with AI are taken back once the last client is gone. Clients that never open a long-lived stream cannot be proven closed: after 30 minutes without requests (and no command running) a dialog asks "End AI control / Keep", with Keep as the default focus. Cleanups land in the audit log

Config example (`.omp/mcp.json`, same shape for Claude Code):

```json
{
  "mcpServers": {
    "myssh": {
      "type": "http",
      "url": "http://127.0.0.1:17345/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

## Roadmap

- Agent CLI: scriptable command-line interface (planned)
- Bastion / jump-server navigation: drive menu-style bastion hosts (e.g. JumpServer) to reach target assets (planned)

## Tech Stack

| Layer       | Choice                                              |
| ----------- | --------------------------------------------------- |
| Desktop     | Tauri 2.11 (Rust + WebView2)                        |
| Frontend    | TypeScript + React 19 + Vite + zustand + Tailwind 4 |
| Terminal    | xterm.js 6 + WebGL addon (canvas fallback)          |
| SSH         | russh 0.62 / russh-sftp 2.4                         |
| Local shell | portable-pty (ConPTY)                               |
| Storage     | SQLite (sqlx) + DPAPI credential vault              |
| Async       | tokio                                               |

## Build

Requirements: Rust stable, Node.js 20+, Windows 10/11 (WebView2 Runtime).

```bash
# Frontend
cd app/ui
npm install
npm run build

# Desktop app (release binary)
cargo build -p app --release
# Output: target/release/app.exe

# Installers (NSIS + MSI) — run from repo root
app/ui/node_modules/.bin/tauri.cmd build
# Output: target/release/bundle/
```

Dev mode with hot reload:

```bash
cd app/ui && npm run dev        # Vite dev server
cargo run -p app                # in another terminal
```

## Tests & Quality

```bash
# Rust (repo root)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Frontend (app/ui)
npm run lint
npm test
```

Engineering rules: no `unwrap`/`expect` outside tests (exceptions need a `// SAFETY:` comment), no `unsafe`; terminal output streams over a Tauri IPC Channel (binary, not JSON events); tunnels/SFTP run on dedicated SSH connections isolated from interactive terminals.

## Project Layout

```
crates/
  app/          Tauri shell: command layer, IPC wiring
  core-ssh/     SSH protocol core (connections, auth, channels)
  core-tunnel/  Port forwarding
  core-sftp/    SFTP & transfer queue
  core-monitor/ Server metrics
  core-store/   SQLite persistence, credential vault, import/export
  core-policy/  Policy
  cli/          Agent CLI (planned)
app/ui/         Frontend (React + xterm)
```

## Data Location

`%LOCALAPPDATA%\myssh\`: `myssh.db` (SQLite: sessions, tunnels, transfer history), credentials (DPAPI-encrypted, bound to the current Windows user and machine), `known_hosts`, `logs/`.

## License

[MIT License](LICENSE)
