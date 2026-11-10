# mySSH AI Integration (MCP)

English | [简体中文](ai-mcp.md)

mySSH ships with a built-in MCP server. Once an AI coding assistant such as Claude Code, Cursor, OpenCode or Droid is connected, it can run commands, read and write files, transfer files and drive interactive terminals on the sessions you have saved.

You decide what the AI may do and when it has to ask you first. Credentials never leave your machine: the AI sees session names and host details, never passwords or private keys.

- [Enable and connect](#enable-and-connect)
- [Tools](#tools)
- [Permissions: which tools the AI may use](#permissions-which-tools-the-ai-may-use)
- [Command policy: which commands need your approval](#command-policy-which-commands-need-your-approval)
- [The approval popup](#the-approval-popup)
- [Seeing what the AI is doing](#seeing-what-the-ai-is-doing)
- [Automatic cleanup when the AI disconnects](#automatic-cleanup-when-the-ai-disconnects)
- [Audit, recordings and rollback](#audit-recordings-and-rollback)

## Enable and connect

1. Open Settings → MCP and check "Enable MCP server" (off by default).
2. Click "Copy config for" and pick your client (omp, Claude Code, OpenCode, or the generic format).
3. Paste it into the client's MCP config file and restart the client.

The server listens on `127.0.0.1` only (default port 17345) and requires a Bearer token. You can copy or regenerate the token in Settings.

The generic format (understood by Claude Code, Cursor, Droid and most other clients):

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

## Tools

31 tools, grouped by purpose:

| Group | Tools | Purpose |
| --- | --- | --- |
| Sessions | `list_sessions` | List session profiles (no credentials), with each session's AI permissions, open tabs, and the terminals the AI can use right away |
| Run commands | `ssh_exec` | Run one command and get its output and exit code. Color codes are stripped and long output keeps head and tail, to save tokens |
| Ops recipes | `recipe_list` / `recipe_run` | 16 built-in templates: system overview, disk usage, top processes, ports, service status and logs, Docker containers, service restart and more. Each has a risk level; parameters are validated before use |
| Background jobs | `job_start` / `job_read` / `job_stop` / `job_list` | Run long commands (builds, `tail -f`, large downloads) in the background, read output in chunks, stop any time |
| Interactive terminals | `terminal_open` / `terminal_send` / `terminal_read` / `terminal_list` / `terminal_close` | A stateful shell for REPLs, pagers and anything `ssh_exec` can't drive. `terminal_read` can wait for a prompt or for output to go quiet |
| Browse files | `sftp_home` / `sftp_list` / `sftp_stat` / `sftp_read` | Browse remote directories and read files |
| Change files | `sftp_write` / `sftp_mkdir` / `sftp_delete` / `sftp_rename` / `sftp_chmod` | Write, create directories, delete, rename, change permissions |
| Transfer files | `sftp_upload` / `sftp_download` / `sftp_transfer_list` | Uses the background transfer queue shared with the UI transfer panel; no size limit |
| Edit files | `file_view` / `file_edit` / `file_grep` / `file_glob` | Paged view with line numbers, exact replace edits (guarded against overwriting concurrent changes), regex search, glob find. All over SFTP, no remote command execution |
| Backup and rollback | `file_backups` / `file_rollback` | Original content is backed up before the AI changes a file; roll back with one click |

## Permissions: which tools the AI may use

The tools fall into 6 permission groups: session list, run commands (including background jobs and recipes), SFTP read, SFTP write, SFTP transfer, and interactive terminals.

Each session's permissions are resolved session → its group → global, the most specific setting wins:

- **Global defaults**: Settings → MCP holds the 6 group toggles and the access level.
- **Group policy**: right-click a group in the sidebar → "MCP Policy". Subgroups and their sessions inherit it.
- **Per-session override**: the "MCP Permissions" tab in the session editor; every item can inherit or be set on its own.

A typical setup: everything allowed on test boxes, view-only on production.

Interactive terminals are off by default, because keystrokes typed into a terminal cannot be checked by the command policy. Only turn them on for an AI you trust.

## Command policy: which commands need your approval

Every command the AI runs through `ssh_exec`, background jobs or recipes is analyzed first. The result decides whether it runs right away or asks you.

**Access levels** (default: read-only):

| Level | Runs right away | Needs your approval | Refused |
| --- | --- | --- | --- |
| Read-only | Provably read-only commands (whitelisted, with arguments that don't change the system) | The always-ask commands below | Everything else |
| Standard | Provably read-only commands | Everything else | Nothing |
| Full | Everything except the always-ask commands | The always-ask commands | Nothing |

These always need your approval, at any level:

- blacklisted commands such as `rm`, `dd`, `mkfs`;
- indirect executors, programs that run other commands or scripts, such as `bash`, `xargs`, `sudo`;
- constructs that cannot be analyzed statically, such as subshells or unbalanced quotes;
- on sessions marked "production", every command that is not provably read-only.

Prefixed commands such as `sudo rm` are checked by the command they actually run, so a prefix cannot sneak past the blacklist.

The three lists (whitelist, blacklist, indirect executors) can be edited under Settings → Command policy, each with "Restore defaults". Changes apply to the next command without a restart.

## The approval popup

Commands that need approval open a popup inside mySSH:

- **Which machine**: session name, user and host, group, and a red marker for production. One click jumps to that session's terminal.
- **What the command does**: one sentence sums up the whole command, followed by why it needs approval (for example "may change the system: git pull, npm, pm2"). The purpose the AI gave is shown too, marked as unverified.
- **Deny is the default**: focus starts on "Deny", which shows the seconds left; after 5 minutes the request is denied automatically.
- **Auto-run**: check "Auto-run (later commands on this session)" and later commands from this AI on this session still show the popup, but pre-checked, and run after 3 seconds unless you cancel. Blacklisted commands can never auto-run. Auto-run ends when the AI client disconnects.
- **Several sessions at once**: a left column groups requests by session. You can "Deny all for this session", but approvals are one at a time.
- The taskbar flashes when the window is in the background. With no app window available, requests are denied.

Each session allows 60 calls per minute by default (setting key `mcp.rate_limit_per_minute`).

## Seeing what the AI is doing

- **Command mirror**: commands the AI runs with `ssh_exec`, their live output and exit code appear in a read-only split pane under that session's terminal, without touching the terminal you are using. Turn it off in Settings → MCP.
- **AI terminals are normal tabs**: terminals the AI opens show up as regular tabs you can watch. The AI pauses while you type, and you can right-click to take back control any time. You can also "Share with AI" one of your own tabs.
- **AI activity panel**: the "AI" toolbar button lists pending approvals, running commands, background jobs and AI terminals per session, each with a stop button. Tabs and sidebar rows show "AI running" / "pending" badges.

## Automatic cleanup when the AI disconnects

Each AI client gets its own session id when it connects, and only its own leftovers are cleaned up when it goes away:

- its pending approval popups are denied and closed;
- its running commands and background jobs are stopped;
- its AI terminals are released (the tabs stay open, only AI control is taken back), and command mirrors no other client uses are closed;
- a notice lists what was cleaned up.

A client counts as gone when its long-lived stream drops and does not reconnect within 30 seconds, or when it ends its session. Clients that never keep a stream open can't be proven gone: after 30 minutes without requests, a dialog asks whether to "End AI control" or "Keep".

## Audit, recordings and rollback

- **Audit log**: every AI call and every decision you make is written to the audit log. Entries are hash-chained, so later tampering can be detected.
- **Terminal recordings**: AI-controlled terminals are recorded from start to finish, including what the AI typed. When a recording ends its SHA-256 goes into the audit log. Command Palette → "Session Recordings" plays recordings back and verifies they were not modified.
- **File rollback**: `file_edit`, `sftp_write` and `sftp_delete` back up the original file first. Right-click a session → "AI File Changes…" shows each change with a title and summary, with one-click rollback. If someone changed the file after the AI did, rollback is refused by default so their work isn't overwritten.
- **Retention**: backups are kept for 7 days and the audit log for 30 days by default; adjust under Settings → General → History Retention.
