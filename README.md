# ustc-107-ssh

`ustc-107-ssh` is a small Rust CLI for making the USTC 107 Web Shell usable from local terminal workflows.

Current state: **protocol-probe / attach MVP**, not a full SSH bridge yet.

The long-term target is:

```text
local ssh client
  -> 127.0.0.1:<port> local SSH-compatible bridge
  -> wss://107.ustc.edu.cn/api/shell?... WebSocket
  -> USTC 107 login node
```

The first version deliberately starts smaller:

- `probe`: connect to the 107 WebSocket with your existing browser Cookie and test one command;
- `attach`: connect your local terminal stdio to the 107 WebSocket stream;
- `url`: print the exact WebSocket URL for a cluster/login-node pair;
- `doctor`: check local prerequisites and safety assumptions;
- `skill`: print compact instructions for AI agents.

## Why Rust CLI first

This tool handles authentication cookies, local network listeners, WebSocket streams, and eventually an SSH-like local bridge. The core should therefore be:

- small;
- auditable;
- dependency-light;
- strict about TLS;
- conservative about logging secrets;
- suitable for long-running CLI/daemon usage.

A GUI or browser-login helper can be added later, but the core bridge should remain a Rust CLI.

## WebSocket URL shape

Observed after logging into `https://107.ustc.edu.cn/` and opening:

```text
https://107.ustc.edu.cn/shell/training/11.11.10.202
```

The browser opens:

```text
wss://107.ustc.edu.cn/api/shell?cluster=training&loginNode=11.11.10.202&path=&cols=80&rows=24&useRoot=false
```

The WebSocket uses normal browser headers including:

```text
Origin: https://107.ustc.edu.cn
Cookie: SCOW_USER=...
```

Do **not** commit real cookies to this repository.

## Install / build

```bash
cargo build --release
./target/release/ustc-107-ssh --help
```

## Cookie input

The CLI accepts cookies in three ways:

```bash
# environment variable
export USTC_107_COOKIE='SCOW_USER=...; scow-dark=...'
ustc-107-ssh probe

# prompt without echo
ustc-107-ssh probe --cookie-stdin

# file, preferably chmod 600
ustc-107-ssh probe --cookie-file ~/.config/ustc-107-ssh/cookie.txt
```

The tool never prints the cookie value. It only reports whether a cookie was supplied and its redacted length.

## Probe

```bash
ustc-107-ssh probe \
  --cluster training \
  --login-node 11.11.10.202 \
  --command 'echo USTC_107_SSH_PROBE'
```

This connects to the WebSocket, waits briefly for shell output, sends the command, then prints received frames.

Current protocol assumption:

```json
{"$case":"data","data":{"data":"..."}}
```

Frames of `$case=exit|close|logout` are treated as remote-session termination signals. Unknown frames are printed in debug form.

## Attach

```bash
ustc-107-ssh attach --cluster training --login-node 11.11.10.202
```

`attach` pipes local stdin to the WebSocket and WebSocket output to stdout.

This is a raw terminal stream, not a full SSH session yet. It is useful for validating the protocol before implementing the local SSH server.

## Doctor

```bash
ustc-107-ssh doctor
```

Checks:

- OS;
- whether a cookie source is present;
- generated WebSocket URL;
- whether unsafe TLS mode is disabled.

## Security model

Hard defaults:

- TLS certificate verification is enabled by default.
- Cookie values are never logged.
- The MVP does not store cookies by itself.
- Future local listeners must default to `127.0.0.1` only.
- Future LAN listening must require an explicit `--allow-lan` style flag.

Non-goals of the MVP:

- automatic SSO login;
- GUI;
- complete SSH protocol compatibility;
- bypassing USTC policy or access control;
- background persistence of credentials.

## Future SSH bridge plan

After `probe` and `attach` are confirmed against the live service, see `docs/ssh-bridge-design.md` for the bridge contract.

1. Add a persistent local host key under `~/.config/ustc-107-ssh/host_key`.
2. Add local SSH server support using a Rust SSH server crate.
3. Accept only local loopback clients by default.
4. Provide `print-ssh-config`:

   ```sshconfig
   Host ustc107
     HostName 127.0.0.1
     Port 3000
     StrictHostKeyChecking accept-new
   ```

5. Implement shell mode first; treat `ssh host 'cmd'` exec as best-effort only if the WebShell protocol has no native exec.
6. Add explicit timeout and output-bound behavior for exec emulation.

## Agent skill

For agent-facing instructions, run:

```bash
ustc-107-ssh skill
```

or read:

```text
skills/ustc-107-ssh/SKILL.md
```

## Legal / acceptable use

This project is for learning, research, and authorized access to USTC 107 resources. Users must comply with USTC and platform rules. This is not an official USTC project.
