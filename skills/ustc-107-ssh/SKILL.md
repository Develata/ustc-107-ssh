# ustc-107-ssh Agent Skill

Use this skill when an AI agent needs terminal access to the USTC 107 Web Shell through the local `ustc-107-ssh` bridge.

## Status

This project is initially a Rust CLI protocol probe / attach tool. Treat full SSH compatibility as a future feature unless the local bridge command explicitly says it is running.

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

The user supplies the browser `Cookie:` header through one of:

```bash
export USTC_107_COOKIE='...'
ustc-107-ssh probe

ustc-107-ssh probe --cookie-stdin

ustc-107-ssh probe --cookie-file ~/.config/ustc-107-ssh/cookie.txt
```

## Probe First

Before assuming bridge availability, run:

```bash
ustc-107-ssh doctor
ustc-107-ssh probe --command 'echo USTC_107_SSH_PROBE'
```

Success means the WebSocket protocol and cookie work. It does not yet prove full SSH behavior.

## Attach Mode

For raw terminal validation:

```bash
ustc-107-ssh attach
```

This pipes local stdin/stdout to the WebSocket data frames. It is not a full SSH server.

## Future SSH Mode

When implemented, the expected pattern is:

```bash
ustc-107-ssh serve --listen 127.0.0.1:3000
ssh -p 3000 127.0.0.1
```

If `serve` is unavailable, do not pretend SSH is ready; use `probe` / `attach` only.

## Troubleshooting

- `401/403` or policy close: cookie expired; user must log in again.
- TLS failure: do not disable verification by default; inspect corporate proxy / clock / certificate chain.
- No prompt output: verify the WebSocket URL query parameters: cluster, loginNode, path, cols, rows, useRoot.
- Garbled terminal behavior: attach is raw stream; full PTY/SSH handling is a later layer.
