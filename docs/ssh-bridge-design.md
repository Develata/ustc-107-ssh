# SSH Bridge Design Notes

This document defines the local SSH bridge layer implemented by `ustc-107-ssh serve`.

## Contract

The command shape is:

```bash
ustc-107-ssh serve --listen 127.0.0.1:3000
ssh -p 3000 127.0.0.1
```

The bridge is not a real remote `sshd`; it is a local compatibility layer over SCOW WebShell.

## Security defaults

- Bind only to `127.0.0.1` by default.
- Reject non-loopback listen addresses unless `--allow-lan` is explicitly set.
- Generate and persist a local host key under the user's config directory with private-file permissions.
- Do not accept public LAN traffic by accident.
- Do not log credentials, cookies, terminal input, or terminal output by default.
- Keep TLS verification enabled for the upstream WebSocket.

Current local authentication accepts `none`, password, and publickey from the local SSH client. This is deliberately scoped to the loopback trust boundary; it must not be exposed to LAN without an explicit risk decision.

## Protocol mapping

```text
SSH channel data <-> WebSocket JSON frame { "$case": "data", "data": { "data": "..." } }
SSH PTY/window size <-> WebSocket JSON frame { "$case": "resize", "resize": { "cols": N, "rows": M } }
```

Remote output is decoded from `$case=data`. Unknown frames are logged only at debug level. Cookie and terminal payloads are not logged by default.

The frame shape is grounded in OpenSCOW's `apps/portal-web/src/server/setup/shell.ts` and `pageComponents/shell/Shell.tsx`:

- Browser sends `$case=data` for xterm input.
- Browser sends `$case=resize` on fit/window changes.
- Server sends `$case=data` and `$case=exit`.

## Shell vs exec

Shell mode is first-class: a client shell request starts a WebShell connection and forwards channel bytes.

Exec mode is best-effort because SCOW WebShell exposes an interactive terminal stream, not a real SSH exec primitive. Current behavior:

1. accept the SSH exec request;
2. start the same WebShell bridge;
3. write `command + "\n"` into the interactive stream.

This does not guarantee a reliable exit status. A later hardened exec emulation should add:

- a unique marker;
- timeout;
- output size limit;
- clear failure modes when markers are missing or ambiguous.

## PTY and resize

The bridge propagates initial PTY size through the WebSocket URL:

```text
cols=80&rows=24
```

If the local client reports invalid tiny sizes, the bridge normalizes them to the default `80x24`. The bridge also sends a browser-like initial `$case=resize` after the WebSocket opens, and converts window changes into later `$case=resize` frames.

`probe` mirrors the same browser-ish behavior for protocol debugging: initial resize by default, configurable pre-read, and configurable Enter suffix (`cr`, `lf`, `crlf`, `none`).

## Implementation choice

The SSH server layer uses `russh`:

- maintained Tokio SSH2 implementation;
- server-side shell channel support;
- host key management through `ssh-key` types;
- compatible async model with the existing WebSocket code;
- Apache-2.0 upstream license.

## Current limits

- No SFTP/SCP support.
- No SSH port forwarding.
- No robust exec exit status.
- No native browser-assisted login yet.
- Live end-to-end success still depends on 107 returning `$case=data` frames for the supplied Cookie and target login node. Current live smoke has observed WebSocket `HTTP 101 Switching Protocols` but no `$case=data` output; this is a handshake-vs-terminal-data blocker, not an SSH channel plumbing proof.
