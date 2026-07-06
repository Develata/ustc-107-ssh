# SSH Bridge Design Notes

This document defines the intended local SSH bridge layer. It is intentionally not implemented in the first MVP until `probe` and `attach` validate the live WebSocket protocol.

## Contract

The future command should look like:

```bash
ustc-107-ssh serve --listen 127.0.0.1:3000
ssh -p 3000 127.0.0.1
```

The bridge is not a real remote `sshd`; it is a local compatibility layer over SCOW Web Shell.

## Security defaults

- Bind only to `127.0.0.1` by default.
- Reject non-loopback listen addresses unless an explicit `--allow-lan` flag exists.
- Generate and persist a local host key under the user's config directory with private-file permissions.
- Do not accept public LAN traffic by accident.
- Do not log credentials, cookies, terminal input, or terminal output by default.
- Keep TLS verification enabled for the upstream WebSocket.

## Protocol mapping

```text
SSH channel data <-> WebSocket JSON frame { "$case": "data", "data": { "data": "..." } }
```

Remote output is decoded from `$case=data`. Unknown frames should be logged only at debug level, with secret redaction.

## Shell vs exec

Shell mode should be first-class. Exec mode is only safe if the upstream protocol provides a real exec primitive.

If exec has to be emulated by sending commands into an interactive shell, it must be documented as best-effort and guarded by:

- a unique marker;
- timeout;
- output size limit;
- clear failure modes when markers are missing or ambiguous.

## PTY and resize

The bridge should propagate local PTY size where possible by rebuilding or extending the WebSocket query / resize frames after the live protocol is known.

The URL already carries initial `cols` and `rows`:

```text
cols=80&rows=24
```

## Implementation candidates

Candidate Rust SSH server crates must be evaluated before implementation. Required properties:

- maintained enough for public use;
- supports server-side shell channel;
- supports host key management;
- async integration with tokio;
- clear license.

Do not add a large SSH dependency before the raw `attach` path is proven.
