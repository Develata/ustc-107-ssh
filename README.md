# ustc-107-ssh

`ustc-107-ssh` 是一个 Rust CLI 工具，目标是把中国科学技术大学 107 平台的 Web Shell 接入本地终端与本地 OpenSSH 工作流。

当前状态：**WebShell bridge MVP + Dashboard files API MVP**。

它已经提供本地 SSH server 外壳，把 OpenSSH client 的 shell channel 转发到 107 WebShell WebSocket；但它不是远端真实 `sshd`，`exec`/`scp`/端口转发等 SSH 语义仍是非目标或 best-effort。

```text
local ssh client
  -> 127.0.0.1:<port> 本地 SSH 兼容桥接层
  -> wss://107.ustc.edu.cn/api/shell?... WebSocket
  -> USTC 107 登录节点
```

## CLI 结构

命令采用树形分叉，尽量保持“领域名词 → 动作/检查”的一致性：

```bash
ustc-107-ssh url
ustc-107-ssh doctor --auth-check
ustc-107-ssh login
ustc-107-ssh cookie path
ustc-107-ssh cookie import --cookie-stdin
ustc-107-ssh cookie inspect
ustc-107-ssh probe --sso-login --browser-compatible
ustc-107-ssh attach --sso-login
ustc-107-ssh serve --sso-login --listen 127.0.0.1:3000
ustc-107-ssh files --sso-login ls /home/scc/pb22010333
ustc-107-ssh files --sso-login get /home/scc/pb22010333/remote.txt ./remote.txt
ustc-107-ssh files --sso-login put ./local.txt /home/scc/pb22010333/local.txt
ustc-107-ssh print-ssh-config --host ustc107
```

当前切片包含：

- `url`：打印指定 cluster / login-node 的准确 WebSocket URL；
- `doctor`：检查本地前提条件与安全假设，`--auth-check` 会额外请求 `/api/auth` 判断 Cookie 是否被 107 auth 入口接受；
- `login`：headless 走 USTC 统一身份认证 UsernamePassword flow，成功后导入 107 Cookie；
- `cookie import/path/inspect`：管理本用户的本地 Cookie 文件，不打印 secret 值；
- `probe`：直接 `--sso-login` 或使用已有浏览器 Cookie 连接 107 WebSocket，并测试一条命令；
- `attach`：直接 `--sso-login` 或把本地终端 stdio 接到 107 WebSocket 数据流；
- `serve`：直接 `--sso-login` 或使用 Cookie 启动本地 SSH server，把 SSH shell channel 转发到 107 WebSocket；
- `files ls/get/put/mkdir/rm`：直接 `--sso-login` 或使用 Cookie 调用 107 Dashboard `/api/file/*`，提供最小上传/下载/列目录能力；
- `print-ssh-config`：打印 OpenSSH 配置片段；
- `skill`：打印供 AI agent 阅读的简明操作说明。

## 安装 / 构建

发布版本会随 `v*` tag 自动生成 GitHub Release 资产：

- `ustc-107-ssh-vX.Y.Z-linux-x86_64.tar.gz`
- `ustc-107-ssh-vX.Y.Z-windows-x86_64.zip`
- `ustc-107-ssh-vX.Y.Z-macos-x86_64.tar.gz`
- `ustc-107-ssh-vX.Y.Z-macos-arm64.tar.gz`
- 每个资产都有同名 `.sha256` 校验文件。

源码构建：

```bash
cargo build --release
./target/release/ustc-107-ssh --help
```

本项目默认不需要 root 权限。普通用户可以直接运行：

- 默认监听 `127.0.0.1:3000`，端口大于 1024；
- host key 默认生成到 `~/.config/ustc-107-ssh/host_key`；
- Cookie 默认读取自 `~/.config/ustc-107-ssh/cookie.txt`；
- Unix 下 config 目录权限设为 `0700`，secret 文件设为 `0600`。

## Direct SSO connection

如果已经在环境里提供：

```bash
export USTC_Student_ID='...'
export USTC_PASSWORD='...'
```

后续连接不需要手工复制 Cookie，可以直接在会话命令上加 `--sso-login`：

```bash
ustc-107-ssh probe --sso-login --browser-compatible --pre-read-seconds 8 --read-seconds 12
ustc-107-ssh attach --sso-login
ustc-107-ssh serve --sso-login --listen 127.0.0.1:3000
```

语义：每次命令启动时先走 USTC 统一身份认证，临时拿到 107 WebShell Cookie，然后立即连接 `/api/shell`；不会要求用户先执行 `cookie import`，也不会打印或保存 Cookie。若同时传入 `--sso-login` 与 `--cookie/--cookie-file/--cookie-stdin`，工具会拒绝，因为这两个来源语义互斥。

注意：`--sso-login` 只表示“不手工传 Cookie，直接走统一身份认证”，不表示 CAS 永远不会要求二次验证。当前在 Hermes 环境与 Develata 本地 Windows 手动验证时，UsernamePassword OAuth flow 均未触发短信/电话/OTP，直接获得 107 `SCOW_USER`；但 Develata 实际使用中很多场景会触发二次验证。若 CAS 对某次登录触发短信/电话/OTP/终端绑定，CLI 会检测并明确报出 unsupported extra step；验证码自动提交分支仍需要基于真实触发页面补充。

## Dashboard file transfer

107 的网页文件管理页面：

```text
https://107.ustc.edu.cn/files/training/home/scc/pb22010333
```

背后使用的是 Dashboard 文件 API，而不是 SSH/SFTP。107 官方帮助页也说明：当前平台主线流程以 Web GUI、文件管理、登录集群 Shell 和 Slurm 作业为准；如果页面或课程通知没有提供 SSH 入口，不要假设可以直接用本地 `ssh` 登录。文件传输方面，官方建议优先使用 GUI 文件管理；大目录先本地打包后上传，上传后用文件大小或 `sha256sum` 校验完整性。

因此 CLI 暴露的是一个最小 Dashboard 文件 API 转接层，而不是完整 SFTP server：

```bash
# 默认 cluster=training；不传 path 时 ls 会读取 dashboard home
ustc-107-ssh files --sso-login ls
ustc-107-ssh files --sso-login ls /home/scc/pb22010333

# 下载 / 上传单个文件
ustc-107-ssh files --sso-login get /home/scc/pb22010333/remote.txt ./remote.txt
ustc-107-ssh files --sso-login put ./local.txt /home/scc/pb22010333/local.txt

# 目录和删除
ustc-107-ssh files --sso-login mkdir /home/scc/pb22010333/tmp-dir
ustc-107-ssh files --sso-login rm /home/scc/pb22010333/file.txt
ustc-107-ssh files --sso-login rm --dir /home/scc/pb22010333/tmp-dir
```

实现 contract：

- `files` 命令每次先通过 `--sso-login` 走统一身份认证，临时拿到 Dashboard Cookie；不打印或保存 Cookie；
- `ls` 调用 `GET /api/file/list`；
- `get` 调用 `GET /api/file/download`；
- `put` 调用 `POST /api/file/upload` 的单请求上传路径；
- `mkdir` / `rm` 调用对应的 `/api/file/mkdir`、`deleteFile`、`deleteDir`；
- 这不是完整 SFTP server：递归目录同步、大文件 multipart/chunk resume、权限保留、软链接语义、scp/sftp protocol 兼容暂未实现。

已验证的最小 smoke：`ls home -> mkdir .cache/tmp -> put small file -> get same file -> rm file -> rm --dir tmp`。

## Headless SSO login

推荐路径：

```bash
ustc-107-ssh login
```

行为 contract：

- 用户名在 CLI 明文提示输入；密码无回显输入；也可通过环境变量 `USTC_Student_ID` 与 `USTC_PASSWORD` 提供，便于非交互测试；
- 工具访问 `https://107.ustc.edu.cn/auth/public/ustc/oauth/start`，跟随官方 USTC CAS OAuth redirect；
- 密码按 CAS 前端协议加密后提交：`AES-128-ECB-PKCS7(Base64(login-croypto), plaintext_password)`；不会保存明文；
- 成功后只导出 WebShell 所需的 107 Cookie；不会导出 CAS/中间态 `SESSION`，不会打印 Cookie 值；
- 默认会自动运行一次 browser-compatible `probe` 验证。当前 Hermes 环境与 Develata 本地 Windows 手动验证未触发二次验证，UsernamePassword OAuth flow 可直接获得 107 `SCOW_USER`；但实际使用中很多场景可能触发短信/电话/OTP，工具会在该额外步骤失败并提示。若只想导入 Cookie，不做验证：

```bash
ustc-107-ssh login --no-verify
```

当前限制：如果 USTC CAS 对本次登录要求短信/电话验证码、OTP、终端绑定等二次验证，CLI 会检测并报出 unsupported extra step；后续需要根据真实触发页面/接口补验证码提交分支。

## Cookie 输入与导入

推荐先导入完整 Cookie header：

```bash
ustc-107-ssh cookie import --cookie-stdin
ustc-107-ssh cookie inspect
```

导入后，后续命令默认从：

```bash
ustc-107-ssh cookie path
# ~/.config/ustc-107-ssh/cookie.txt
```

读取 Cookie；也可以显式指定来源：

```bash
# 环境变量
export USTC_107_COOKIE='SCOW_USER=...; ...'
ustc-107-ssh probe

# 无回显输入
ustc-107-ssh probe --cookie-stdin

# 文件，建议 chmod 600
ustc-107-ssh probe --cookie-file ~/.config/ustc-107-ssh/cookie.txt
```

工具不会打印 Cookie 值，只会报告 cookie pair 数量、总长度与 cookie names。

### 如何复制正确的 Cookie

不要从 `document.cookie` 复制；它可能看不到 `HttpOnly` Cookie。推荐从浏览器 DevTools 复制完整请求头：

```text
Network
  -> 过滤 wss://107.ustc.edu.cn/api/shell?... 请求
  -> Request Headers
  -> Cookie: <复制冒号后的完整值>
```

只含 `SCOW_USER`、`scow-dark`、`_ga*` 的 Cookie 也可能足以创建 shell；`doctor --auth-check` 只检查 `/api/auth` 入口形态，不是 WebShell Cookie 的最终有效性判据。若复制了多段 Cookie 导致同名项重复，工具会输出 `cookie_warning: duplicate_cookie_names`，此时应重新从当前活跃的单个 WebSocket 请求复制一条完整 `Cookie:` header。

## WebSocket URL 形态

登录 `https://107.ustc.edu.cn/` 后打开：

```text
https://107.ustc.edu.cn/shell/training/11.11.10.202
```

浏览器实际建立的 WebSocket 为：

```text
wss://107.ustc.edu.cn/api/shell?cluster=training&loginNode=11.11.10.202&path=&cols=80&rows=24&useRoot=false
```

`useRoot=false` 是普通用户默认；只有显式 `--use-root` 时才请求 `useRoot=true`。本工具默认面向非 root 普通用户。

SCOW WebShell frame 形态：

```json
{"$case":"data","data":{"data":"..."}}
{"$case":"resize","resize":{"cols":80,"rows":24}}
{"$case":"disconnect"}
```

## Doctor

基础检查：

```bash
ustc-107-ssh doctor
```

带 auth 入口检查：

```bash
ustc-107-ssh doctor --auth-check
```

输出示例形态：

```text
websocket_url: wss://107.ustc.edu.cn/api/shell?...&useRoot=false
cookie: present (3 cookie pair(s), ... names=[...])
cookie_hint: possibly_incomplete_public_cookies_only
auth_check_status: 307
auth_check_classification: auth_endpoint_redirected_probe_required
auth_check_note: /api/auth redirects can still occur for cookies that work with /api/shell; use probe output as the WebShell validity check
```

注意：`doctor --auth-check` 是辅助诊断；`/api/auth` 的 307 redirect 不足以证明 Cookie 对 WebShell 无效。真正的有效性判据是 `probe` 是否收到远端 `$case=data` prompt/命令输出。`HTTP 101 Switching Protocols` 只证明 WebSocket 握手成功，不证明远端 shell 已经分配成功。

## Probe

普通 probe：

```bash
ustc-107-ssh probe \
  --cluster training \
  --login-node 11.11.10.202 \
  --command 'echo USTC_107_SSH_PROBE'
```

对齐浏览器 / `usshtc-107` 行为的 probe：

```bash
ustc-107-ssh probe --browser-compatible \
  --pre-read-seconds 8 \
  --read-seconds 12 \
  --command 'echo USTC_107_SSH_PROBE'
```

`--browser-compatible` 会：

- 使用 Chrome-like `User-Agent`；
- 默认把 Enter 从 `cr` 调整为 `lf`；
- WebSocket open 后按浏览器行为连续发送两次 initial resize；
- WebSocket open 后至少等待 1000ms 再发送命令；
- 至少读取 12 秒输出。

可调参数：

```bash
ustc-107-ssh probe \
  --pre-read-seconds 8 \
  --read-seconds 12 \
  --enter cr \
  --send-delay-ms 1000 \
  --command 'echo USTC_107_SSH_PROBE'
```

## Attach

```bash
ustc-107-ssh attach --cluster training --login-node 11.11.10.202
```

`attach` 会把本地 stdin 写入 WebSocket，并把 WebSocket 输出写到 stdout。它用于验证 107 WebShell 协议，不是完整 SSH session。

## SSH bridge MVP

启动本地 SSH bridge：

```bash
ustc-107-ssh serve --listen 127.0.0.1:3000
```

另开一个终端连接：

```bash
ssh -p 3000 127.0.0.1
```

也可以打印 OpenSSH 配置片段：

```bash
ustc-107-ssh print-ssh-config --host ustc107 --listen 127.0.0.1:3000
```

输出形如：

```sshconfig
Host ustc107
  HostName 127.0.0.1
  Port 3000
  User webshell
  StrictHostKeyChecking accept-new
```

### 当前 SSH bridge 语义

- 本地监听默认只允许 loopback，例如 `127.0.0.1:3000`。
- 非 loopback 地址会被拒绝；除非显式传入 `--allow-lan`。
- host key 默认生成到 `~/.config/ustc-107-ssh/host_key`，Unix 下目录权限为 `0700`，私钥文件为 `0600`。
- 认证当前接受本地 client 的 `none` / password / publickey；安全边界来自 loopback listener 与本机用户会话，而不是远端 SSH 账号认证。
- shell channel 会转发为 SCOW WebShell 的 JSON data frame。
- PTY 初始尺寸通过 WebSocket URL 的 `cols`/`rows` 传入；连接后会发送一次初始 `$case=resize` frame，窗口变化也会发送 `$case=resize` frame。
- `ssh host 'cmd'` 走 best-effort：它会把命令写入交互 shell，不保证真实 SSH exec exit status。

### 本地 smoke test

```bash
ustc-107-ssh doctor --auth-check
ustc-107-ssh probe --browser-compatible --command 'echo USTC_107_SSH_PROBE'
ustc-107-ssh serve --listen 127.0.0.1:3000
ssh -p 3000 127.0.0.1
```

如果连接后没有远端提示符，先回到 `probe` 和 `attach` 检查 Cookie 是否仍有效，以及 107 WebShell 是否实际返回 `$case=data` frame。

## 安全模型

硬性默认：

- 默认启用 TLS 证书校验；
- Cookie 值永不写入日志；
- Cookie 文件写入当前用户 config 目录，权限尽量收紧；
- 本地监听默认只绑定 `127.0.0.1`；
- 如需 LAN 监听，必须显式提供 `--allow-lan`。

MVP 非目标 / 当前限制：

- 保存统一身份认证密码；
- 自动绕过或代答 MFA / 风控；
- GUI；
- 完整 SSH 协议兼容；
- 绕过 USTC 政策或访问控制。

## 设计来源与致谢

本项目借鉴并参考了 [Enthusjast/usshtc-107](https://github.com/Enthusjast/usshtc-107) 的设计经验，尤其是：

- 从浏览器/Electron session 捕获 107 域名完整 Cookie，而不是只依赖 `document.cookie`；
- WebSocket URL 中的 `useRoot=false` 普通用户模式；
- WebSocket JSON frame：`{"$case":"data","data":{"data":"..."}}`；
- 本地 SSH 兼容层到 107 WebShell 的总体桥接思路。

本项目不是该仓库的 fork，也不是 USTC 官方项目。`usshtc-107` 使用 MIT License；本仓库同样保留独立 MIT 许可，并在实现中避免照搬不适合 CLI 安全模型的部分，例如默认关闭 TLS 校验或日志输出 Cookie 前缀。

## 后续计划

1. 基于真实浏览器 HAR / WebShell frame 继续校准 107 协议细节。
2. 增加更稳健的 `exec` emulation：唯一 marker、timeout、输出上限、明确 exit status 行为。
3. 增加 browser-assisted login，避免手工复制 Cookie。
4. 如果 107 WebShell 支持更丰富的 resize / close / heartbeat 语义，补齐对应 frame。

## Agent skill

供 agent 阅读的说明可以通过命令查看：

```bash
ustc-107-ssh skill
```

也可以直接阅读：

```text
skills/ustc-107-ssh/SKILL.md
```

## 法律 / 可接受使用

本项目仅用于学习、研究，以及在授权范围内访问 USTC 107 资源。用户必须遵守 USTC 与平台规则。本项目不是 USTC 官方项目。
