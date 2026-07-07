# ustc-107-ssh Agent Skill

Use this skill when an AI agent needs terminal access to the USTC 107 Web Shell through the local `ustc-107-ssh` bridge.

## Status

This project has a Rust CLI protocol probe / attach tool and a localhost SSH bridge MVP. The bridge is a compatibility layer over SCOW WebShell, not a real remote `sshd`.

## Safety Rules

1. Never print, log, commit, or store the user's `SCOW_USER` cookie or full `Cookie:` header.
2. Do not ask the user to disable TLS verification. TLS verification is on by default.
3. Do not expose a local bridge on `0.0.0.0` or LAN unless the user explicitly asks and understands the risk.
4. If a command may run on the remote 107 login node, say so before running destructive or expensive actions.
5. Prefer short, observable commands first: `pwd`, `hostname`, `whoami`, `ls`, `echo`.
6. Treat remote GPU/HPC job submission as a separate explicit task; do not start long jobs as a connectivity test.

## Normal User Setup

The user logs into:

```text
https://107.ustc.edu.cn/shell/training/11.11.10.202
```

Observed WebSocket URL:

```text
wss://107.ustc.edu.cn/api/shell?cluster=training&loginNode=11.11.10.202&path=&cols=80&rows=24&useRoot=false
```

The live 107 frontend always includes `useRoot`: `useRoot=false` for normal shell and `useRoot=true` only when explicitly requested and enabled.

The user supplies the browser `Cookie:` header through one of:

```bash
ustc-107-ssh cookie import --cookie-stdin
ustc-107-ssh cookie inspect
ustc-107-ssh probe

export USTC_107_COOKIE='...'
ustc-107-ssh probe

ustc-107-ssh probe --cookie-stdin

ustc-107-ssh probe --cookie-file ~/.config/ustc-107-ssh/cookie.txt
```

Prefer copying the complete `Cookie:` header from DevTools Network → `wss://107.ustc.edu.cn/api/shell?...` → Request Headers. Do not rely on `document.cookie`; it may omit HttpOnly session cookies. Copy exactly one active WebSocket request's Cookie header; if `cookie import` prints `cookie_warning: duplicate_cookie_names`, discard it and recopy a clean single header.

## Probe First

Before assuming bridge availability, run:

```bash
ustc-107-ssh doctor --auth-check
ustc-107-ssh probe --browser-compatible --pre-read-seconds 8 --read-seconds 12 --command 'echo USTC_107_SSH_PROBE'
```

`--browser-compatible` mimics the Electron/browser bridge path: Chrome-like UA, duplicate initial resize frames, LF enter, at least 1s send delay, and a longer read window.

Treat successful `probe` output—remote prompt or command echo/result in `$case=data` frames—as the WebShell validity check. `doctor --auth-check` is only an auxiliary `/api/auth` entrypoint check; a redirect there can still coexist with a working `/api/shell` cookie.

## Attach Mode

For raw terminal validation:

```bash
ustc-107-ssh attach
```

This pipes local stdin/stdout to the WebSocket data frames.

## SSH Bridge Mode

Start the local SSH bridge:

```bash
ustc-107-ssh serve --listen 127.0.0.1:3000
```

Connect from another terminal:

```bash
ssh -p 3000 127.0.0.1
```

Or generate a config snippet:

```bash
ustc-107-ssh print-ssh-config --host ustc107 --listen 127.0.0.1:3000
```

Current bridge semantics:

- listener defaults to loopback and rejects non-loopback unless `--allow-lan` is explicit;
- host key is generated at `~/.config/ustc-107-ssh/host_key` with private permissions;
- local SSH auth accepts `none` / password / publickey because the security boundary is loopback;
- shell mode is primary;
- `ssh host 'cmd'` is best-effort exec emulation through an interactive WebShell, not real SSH exec.

## Troubleshooting

- `doctor --auth-check` returns `307` to `/auth`: cookie is incomplete or expired; import the complete WebSocket request `Cookie:` header from browser DevTools.
- `401/403` or policy close: cookie expired; user must log in again.
- TLS failure: do not disable verification by default; inspect corporate proxy / clock / certificate chain.
- SSH connects but no prompt output: verify that `probe` or `attach` receives `$case=data` output from 107; WebSocket `HTTP 101` alone only proves the handshake.
- If `probe` sees 101 but no `$case=data`, compare against a real browser WebSocket frame capture/HAR; tune `--enter`, `--pre-read-seconds`, and `--no-initial-resize` before changing SSH channel code.
- Tiny `cols=1&rows=1`: local non-PTY smoke tests may report bad terminal size; the bridge normalizes very small values to `80x24`.
- Garbled terminal behavior: remember this is WebShell stream bridging, not a full remote `sshd`.
