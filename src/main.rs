use anyhow::{bail, Context, Result};
use bytes::Bytes;
use clap::{Args, Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use russh::keys::{self, Algorithm, PrivateKey};
use russh::server::{self, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{COOKIE, LOCATION, ORIGIN, USER_AGENT};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
use url::Url;

const DEFAULT_CLUSTER: &str = "training";
const DEFAULT_LOGIN_NODE: &str = "11.11.10.202";
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_ORIGIN: &str = "https://107.ustc.edu.cn";
const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (compatible; ustc-107-ssh/0.1; +https://github.com/Develata/ustc-107-ssh)";
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36 Edg/149.0.0.0";
const ENV_COOKIE: &str = "USTC_107_COOKIE";
const AUTH_CHECK_URL: &str = "https://107.ustc.edu.cn/api/auth";
const DEFAULT_LISTEN: &str = "127.0.0.1:3000";
const CONFIG_DIR_NAME: &str = "ustc-107-ssh";
const HOST_KEY_FILE: &str = "host_key";
const COOKIE_FILE: &str = "cookie.txt";

type WsSink = mpsc::Sender<Vec<u8>>;

struct HostKeyRng;

impl keys::ssh_key::rand_core::TryRng for HostKeyRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> std::result::Result<u32, Self::Error> {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).expect("OS randomness unavailable for SSH host key generation");
        Ok(u32::from_ne_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> std::result::Result<u64, Self::Error> {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).expect("OS randomness unavailable for SSH host key generation");
        Ok(u64::from_ne_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> std::result::Result<(), Self::Error> {
        getrandom::fill(dst).expect("OS randomness unavailable for SSH host key generation");
        Ok(())
    }
}

impl keys::ssh_key::rand_core::TryCryptoRng for HostKeyRng {}

#[derive(Debug, Parser)]
#[command(name = "ustc-107-ssh", version, about)]
struct Cli {
    /// Increase log verbosity. Does not reveal cookies.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the generated WebSocket URL.
    Url(TargetArgs),
    /// Check local assumptions, cookie source presence, and optionally auth status.
    Doctor(DoctorArgs),
    /// Manage local cookie material without printing secret values.
    Cookie {
        #[command(subcommand)]
        command: CookieCommand,
    },
    /// Connect to the WebSocket and send one probe command.
    Probe(ProbeArgs),
    /// Pipe local stdin/stdout to the WebSocket data stream.
    Attach(SessionArgs),
    /// Run a localhost SSH server that bridges shell channels to 107 WebShell.
    Serve(ServeArgs),
    /// Print an OpenSSH config snippet for the local bridge.
    PrintSshConfig(PrintSshConfigArgs),
    /// Print compact agent-facing instructions.
    Skill,
}

#[derive(Debug, Args, Clone)]
struct TargetArgs {
    /// SCOW cluster name from /shell/<cluster>/<login-node>.
    #[arg(long, default_value = DEFAULT_CLUSTER)]
    cluster: String,

    /// Login node IP/name from /shell/<cluster>/<login-node>.
    #[arg(long, default_value = DEFAULT_LOGIN_NODE)]
    login_node: String,

    /// Remote initial path.
    #[arg(long, default_value = "")]
    path: String,

    /// Terminal columns.
    #[arg(long, default_value_t = DEFAULT_COLS)]
    cols: u16,

    /// Terminal rows.
    #[arg(long, default_value_t = DEFAULT_ROWS)]
    rows: u16,

    /// Request root mode from SCOW Web Shell.
    #[arg(long, default_value_t = false)]
    use_root: bool,
}

#[derive(Debug, Args, Clone)]
struct SessionArgs {
    #[command(flatten)]
    target: TargetArgs,

    /// Cookie header value. Prefer env/file/stdin over shell history.
    #[arg(long, env = ENV_COOKIE, hide_env_values = true)]
    cookie: Option<String>,

    /// Read cookie header value from file.
    #[arg(long)]
    cookie_file: Option<PathBuf>,

    /// Prompt for cookie header value on stdin without echo.
    #[arg(long)]
    cookie_stdin: bool,

    /// Origin header.
    #[arg(long, default_value = DEFAULT_ORIGIN)]
    origin: String,

    /// User-Agent header.
    #[arg(long, default_value = DEFAULT_USER_AGENT)]
    user_agent: String,

    /// Allow insecure TLS. This is for debugging only and is intentionally not implemented yet.
    #[arg(long, hide = true, default_value_t = false)]
    insecure_tls: bool,

    /// Do not send a browser-like initial resize frame after WebSocket open.
    #[arg(long, default_value_t = false)]
    no_initial_resize: bool,
}

#[derive(Debug, Args, Clone)]
struct DoctorArgs {
    #[command(flatten)]
    session: SessionArgs,

    /// Also check whether the cookie is accepted by the 107 auth entrypoint.
    #[arg(long, default_value_t = false)]
    auth_check: bool,
}

#[derive(Debug, Subcommand)]
enum CookieCommand {
    /// Import a complete Cookie header into the per-user config file.
    Import(CookieImportArgs),
    /// Print the default cookie file path.
    Path,
    /// Inspect cookie names/count without printing values.
    Inspect(SessionArgs),
}

#[derive(Debug, Args)]
struct CookieImportArgs {
    /// Cookie header value. Prefer file/stdin over shell history.
    #[arg(long, env = ENV_COOKIE, hide_env_values = true)]
    cookie: Option<String>,

    /// Read cookie header value from file.
    #[arg(long)]
    cookie_file: Option<PathBuf>,

    /// Prompt for cookie header value on stdin without echo.
    #[arg(long)]
    cookie_stdin: bool,

    /// Destination file. Defaults to ~/.config/ustc-107-ssh/cookie.txt.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
struct ProbeArgs {
    #[command(flatten)]
    session: SessionArgs,

    /// Command to send after the WebSocket opens.
    #[arg(long, default_value = "echo USTC_107_SSH_PROBE")]
    command: String,

    /// Seconds to read frames after sending the command.
    #[arg(long, default_value_t = 5)]
    read_seconds: u64,

    /// Seconds to read frames before sending the command. Browser terminals often wait for initial shell output first.
    #[arg(long, default_value_t = 3)]
    pre_read_seconds: u64,

    /// Enter key encoding appended to --command: cr, lf, crlf, or none.
    #[arg(long, default_value = "cr")]
    enter: String,

    /// Mimic the browser/Electron bridge behavior: Chrome UA, LF enter, 1s send delay, and longer read window.
    #[arg(long, default_value_t = false)]
    browser_compatible: bool,

    /// Delay before sending the command after the WebSocket opens. Browser-compatible mode uses at least 1000ms.
    #[arg(long, default_value_t = 0)]
    send_delay_ms: u64,
}

#[derive(Debug, Args, Clone)]
struct ServeArgs {
    #[command(flatten)]
    session: SessionArgs,

    /// Local SSH listen address. Defaults to loopback only.
    #[arg(long, default_value = DEFAULT_LISTEN)]
    listen: SocketAddr,

    /// Allow binding to non-loopback addresses. Dangerous: exposes the local bridge to the LAN.
    #[arg(long, default_value_t = false)]
    allow_lan: bool,

    /// OpenSSH private host key path. Generated with 0600 permissions if absent.
    #[arg(long)]
    host_key: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
struct PrintSshConfigArgs {
    /// Host alias to print.
    #[arg(long, default_value = "ustc107")]
    host: String,

    /// Local SSH bridge listen address.
    #[arg(long, default_value = DEFAULT_LISTEN)]
    listen: SocketAddr,
}

#[derive(Clone)]
struct BridgeSettings {
    session: SessionArgs,
    cookie: Arc<String>,
}

#[derive(Clone, Default)]
struct BridgeState {
    channels: Arc<Mutex<HashMap<ChannelId, WsSink>>>,
    pty_sizes: Arc<Mutex<HashMap<ChannelId, (u16, u16)>>>,
}

#[derive(Clone)]
struct SshBridgeServer {
    settings: Arc<BridgeSettings>,
    state: BridgeState,
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls ring crypto provider"))?;

    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Url(args) => {
            println!("{}", build_ws_url(&args)?);
        }
        Command::Doctor(args) => doctor(args).await?,
        Command::Cookie { command } => cookie_command(command)?,
        Command::Probe(args) => probe(args).await?,
        Command::Attach(args) => attach(args).await?,
        Command::Serve(args) => serve(args).await?,
        Command::PrintSshConfig(args) => print_ssh_config(args),
        Command::Skill => print_skill(),
    }
    Ok(())
}

fn init_tracing(verbose: bool) {
    let default = if verbose { "debug" } else { "info" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();
}

fn build_ws_url(args: &TargetArgs) -> Result<Url> {
    let mut url = Url::parse("wss://107.ustc.edu.cn/api/shell")?;
    url.query_pairs_mut()
        .append_pair("cluster", &args.cluster)
        .append_pair("loginNode", &args.login_node)
        .append_pair("path", &args.path)
        .append_pair("cols", &args.cols.to_string())
        .append_pair("rows", &args.rows.to_string())
        .append_pair("useRoot", if args.use_root { "true" } else { "false" });
    Ok(url)
}

fn resolve_cookie(args: &SessionArgs) -> Result<String> {
    let mut sources = 0;
    sources += usize::from(args.cookie.is_some());
    sources += usize::from(args.cookie_file.is_some());
    sources += usize::from(args.cookie_stdin);
    if sources > 1 {
        bail!(
            "provide at most one cookie source: --cookie, --cookie-file, --cookie-stdin, or env {ENV_COOKIE}"
        );
    }

    let cookie = if let Some(value) = &args.cookie {
        value.clone()
    } else if let Some(path) = &args.cookie_file {
        std::fs::read_to_string(path)
            .with_context(|| format!("read cookie file {}", path.display()))?
    } else if args.cookie_stdin {
        rpassword::prompt_password("Cookie header: ")?
    } else {
        let path = default_cookie_path()?;
        std::fs::read_to_string(&path).with_context(|| {
            format!(
                "read default cookie file {}; alternatively pass --cookie-file, --cookie-stdin, or env {ENV_COOKIE}",
                path.display()
            )
        })?
    };

    let cookie = cookie.trim().to_string();
    if cookie.is_empty() {
        bail!("cookie is empty");
    }
    Ok(cookie)
}

fn cookie_names(cookie: &str) -> Vec<String> {
    cookie
        .split(';')
        .filter_map(|part| {
            part.trim()
                .split_once('=')
                .map(|(name, _)| name.trim().to_string())
        })
        .filter(|name| !name.is_empty())
        .collect()
}

fn duplicate_cookie_names(cookie: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut duplicates = Vec::new();
    for name in cookie_names(cookie) {
        if !seen.insert(name.clone()) && !duplicates.contains(&name) {
            duplicates.push(name);
        }
    }
    duplicates
}

fn print_cookie_warnings(cookie: &str) {
    let duplicates = duplicate_cookie_names(cookie);
    if !duplicates.is_empty() {
        println!(
            "cookie_warning: duplicate_cookie_names names={duplicates:?}; copy exactly one Cookie header from the active WebSocket request"
        );
    }
}

fn cookie_summary(cookie: &str) -> String {
    let names = cookie_names(cookie);
    format!(
        "{} cookie pair(s), {} byte(s), names={:?}",
        names.len(),
        cookie.len(),
        names
    )
}

fn cookie_hint(cookie: &str) -> &'static str {
    let names = cookie_names(cookie);
    let has_scow_user = names.iter().any(|name| name == "SCOW_USER");
    let likely_only_public = names
        .iter()
        .all(|name| name == "SCOW_USER" || name == "scow-dark" || name.starts_with("_ga"));
    if !has_scow_user {
        "missing_scow_user"
    } else if likely_only_public {
        "possibly_incomplete_public_cookies_only"
    } else {
        "has_non_public_or_extra_cookies"
    }
}

async fn doctor(args: DoctorArgs) -> Result<()> {
    if args.session.insecure_tls {
        bail!("--insecure-tls is intentionally not supported in this MVP");
    }
    let url = build_ws_url(&args.session.target)?;
    println!("os: {}", std::env::consts::OS);
    println!("arch: {}", std::env::consts::ARCH);
    println!("websocket_url: {url}");
    println!("origin: {}", args.session.origin);
    println!("tls_verification: enabled");
    match resolve_cookie(&args.session) {
        Ok(cookie) => {
            println!("cookie: present ({})", cookie_summary(&cookie));
            print_cookie_warnings(&cookie);
            println!("cookie_hint: {}", cookie_hint(&cookie));
            if args.auth_check {
                auth_check(&args.session, &cookie).await?;
            }
        }
        Err(err) => println!("cookie: missing or invalid ({err})"),
    }
    Ok(())
}

async fn auth_check(args: &SessionArgs, cookie: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(&args.user_agent)
        .build()
        .context("build auth-check HTTP client")?;
    let response = client
        .get(AUTH_CHECK_URL)
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::ORIGIN, &args.origin)
        .send()
        .await
        .context("GET /api/auth")?;
    let status = response.status();
    let location = response
        .headers()
        .get(LOCATION.as_str())
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    println!("auth_check_url: {AUTH_CHECK_URL}");
    println!("auth_check_status: {}", status.as_u16());
    if !location.is_empty() {
        println!("auth_check_location: {location}");
    }
    let classification = if status.is_redirection() && location.contains("/auth") {
        "auth_endpoint_redirected_probe_required"
    } else if status.as_u16() == 401 || status.as_u16() == 403 {
        "auth_endpoint_rejected_cookie_probe_required"
    } else if status.is_success() {
        "accepted_by_auth_endpoint"
    } else {
        "unknown_auth_status_probe_required"
    };
    println!("auth_check_classification: {classification}");
    if status.is_redirection() && location.contains("/auth") {
        println!(
            "auth_check_note: /api/auth redirects can still occur for cookies that work with /api/shell; use probe output as the WebShell validity check"
        );
    }
    Ok(())
}

fn cookie_command(command: CookieCommand) -> Result<()> {
    match command {
        CookieCommand::Import(args) => cookie_import(args),
        CookieCommand::Path => {
            println!("{}", default_cookie_path()?.display());
            Ok(())
        }
        CookieCommand::Inspect(args) => {
            let cookie = resolve_cookie(&args)?;
            println!("cookie: present ({})", cookie_summary(&cookie));
            print_cookie_warnings(&cookie);
            println!("cookie_hint: {}", cookie_hint(&cookie));
            Ok(())
        }
    }
}

fn resolve_cookie_import(args: &CookieImportArgs) -> Result<String> {
    let session_args = SessionArgs {
        target: TargetArgs {
            cluster: DEFAULT_CLUSTER.to_string(),
            login_node: DEFAULT_LOGIN_NODE.to_string(),
            path: String::new(),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            use_root: false,
        },
        cookie: args.cookie.clone(),
        cookie_file: args.cookie_file.clone(),
        cookie_stdin: args.cookie_stdin,
        origin: DEFAULT_ORIGIN.to_string(),
        user_agent: DEFAULT_USER_AGENT.to_string(),
        insecure_tls: false,
        no_initial_resize: false,
    };
    resolve_cookie(&session_args)
}

fn cookie_import(args: CookieImportArgs) -> Result<()> {
    let cookie = resolve_cookie_import(&args)?;
    let path = args.output.unwrap_or(default_cookie_path()?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create cookie dir {}", parent.display()))?;
        set_private_dir_permissions(parent)?;
    }
    std::fs::write(&path, format!("{}\n", cookie.trim()))
        .with_context(|| format!("write cookie file {}", path.display()))?;
    set_private_file_permissions(&path)?;
    println!("cookie_file: {}", path.display());
    println!("cookie: imported ({})", cookie_summary(&cookie));
    print_cookie_warnings(&cookie);
    println!("cookie_hint: {}", cookie_hint(&cookie));
    Ok(())
}

async fn connect_shell(
    args: &SessionArgs,
    cookie: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    if args.insecure_tls {
        bail!("--insecure-tls is intentionally not supported in this MVP");
    }
    let url = build_ws_url(&args.target)?;
    let mut req = url.as_str().into_client_request()?;
    let headers = req.headers_mut();
    headers.insert(ORIGIN, args.origin.parse()?);
    headers.insert(USER_AGENT, args.user_agent.parse()?);
    headers.insert(COOKIE, cookie.parse()?);

    info!("connecting to {url}");
    debug!("cookie supplied: {}", cookie_summary(cookie));
    let (ws, response) = connect_async(req).await.context("connect websocket")?;
    info!("websocket connected: HTTP {}", response.status());
    Ok(ws)
}

async fn send_initial_resize_if_enabled(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    args: &SessionArgs,
) -> Result<()> {
    if args.no_initial_resize {
        return Ok(());
    }
    ws.send(encode_resize_frame(args.target.cols, args.target.rows))
        .await
        .context("send initial resize frame")
}

async fn probe(args: ProbeArgs) -> Result<()> {
    let mut session = args.session.clone();
    let mut enter = args.enter.clone();
    let mut read_seconds = args.read_seconds;
    let mut send_delay_ms = args.send_delay_ms;
    if args.browser_compatible {
        session.user_agent = BROWSER_USER_AGENT.to_string();
        if enter == "cr" {
            enter = "lf".to_string();
        }
        read_seconds = read_seconds.max(12);
        send_delay_ms = send_delay_ms.max(1000);
    }

    let cookie = resolve_cookie(&session)?;
    eprintln!("cookie: {}", cookie_summary(&cookie));
    if args.browser_compatible {
        eprintln!(
            "browser-compatible: enabled (Chrome UA, enter={enter}, send_delay_ms={send_delay_ms})"
        );
    }
    let mut ws = connect_shell(&session, &cookie).await?;
    send_initial_resize_if_enabled(&mut ws, &session).await?;
    if args.browser_compatible && !session.no_initial_resize {
        ws.send(encode_resize_frame(
            session.target.cols,
            session.target.rows,
        ))
        .await
        .context("send duplicate browser-compatible resize frame")?;
    }

    if args.pre_read_seconds > 0 {
        eprintln!("pre-reading for {} second(s)...", args.pre_read_seconds);
        read_ws_messages_for(&mut ws, Duration::from_secs(args.pre_read_seconds)).await?;
    }

    if send_delay_ms > 0 {
        eprintln!("waiting {send_delay_ms}ms before sending command...");
        tokio::time::sleep(Duration::from_millis(send_delay_ms)).await;
    }
    let command = format_probe_command(&args.command, &enter)?;
    ws.send(encode_data_frame(&command))
        .await
        .context("send probe command")?;
    eprintln!("sent command: {:?} with enter={enter}", args.command);

    read_ws_messages_for(&mut ws, Duration::from_secs(read_seconds)).await?;
    let _ = ws.close(None).await;
    Ok(())
}

async fn read_ws_messages_for(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    duration: Duration,
) -> Result<()> {
    let deadline = tokio::time::sleep(duration);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            msg = ws.next() => {
                match msg {
                    Some(Ok(msg)) => print_ws_message(msg)?,
                    Some(Err(err)) => bail!("websocket error: {err}"),
                    None => break,
                }
            }
        }
    }
    Ok(())
}

fn format_probe_command(command: &str, enter: &str) -> Result<String> {
    let suffix = match enter {
        "cr" => "\r",
        "lf" => "\n",
        "crlf" => "\r\n",
        "none" => "",
        other => bail!("unsupported --enter {other:?}; expected cr, lf, crlf, or none"),
    };
    if suffix.is_empty() || command.ends_with('\n') || command.ends_with('\r') {
        Ok(command.to_string())
    } else {
        Ok(format!("{command}{suffix}"))
    }
}

async fn attach(args: SessionArgs) -> Result<()> {
    let cookie = resolve_cookie(&args)?;
    eprintln!("cookie: {}", cookie_summary(&cookie));
    let mut ws = connect_shell(&args, &cookie).await?;
    send_initial_resize_if_enabled(&mut ws, &args).await?;
    let (mut sink, mut stream) = ws.split();

    let stdin_task = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            let n = stdin.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            sink.send(encode_data_frame(&text)).await?;
        }
        Result::<()>::Ok(())
    });

    let stdout_task = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = stream.next().await {
            match msg? {
                Message::Text(text) => {
                    if let Some(data) = decode_data_payload(&text) {
                        stdout.write_all(data.as_bytes()).await?;
                        stdout.flush().await?;
                    } else {
                        debug!("non-data text frame: {text}");
                    }
                }
                Message::Binary(bytes) => {
                    stdout.write_all(&bytes).await?;
                    stdout.flush().await?;
                }
                Message::Close(frame) => {
                    warn!("websocket closed: {frame:?}");
                    break;
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Frame(_) => {}
            }
        }
        Result::<()>::Ok(())
    });

    tokio::select! {
        res = stdin_task => res??,
        res = stdout_task => res??,
        _ = tokio::signal::ctrl_c() => {
            warn!("received Ctrl-C");
        }
    }
    Ok(())
}

async fn serve(args: ServeArgs) -> Result<()> {
    enforce_listen_safety(args.listen, args.allow_lan)?;
    let cookie = resolve_cookie(&args.session)?;
    let host_key_path = args.host_key.clone().unwrap_or(default_host_key_path()?);
    let host_key = load_or_generate_host_key(&host_key_path)?;

    let config = server::Config {
        inactivity_timeout: Some(Duration::from_secs(3600)),
        auth_rejection_time: Duration::from_millis(100),
        auth_rejection_time_initial: Some(Duration::from_millis(0)),
        keys: vec![host_key],
        ..Default::default()
    };
    let config = Arc::new(config);
    let settings = Arc::new(BridgeSettings {
        session: args.session,
        cookie: Arc::new(cookie),
    });
    let mut server = SshBridgeServer {
        settings,
        state: BridgeState::default(),
    };

    let socket = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind SSH bridge listener {}", args.listen))?;
    info!(
        "serving SSH bridge on {} using host key {}",
        args.listen,
        host_key_path.display()
    );
    eprintln!("SSH bridge listening on {}", args.listen);
    eprintln!("Try: ssh -p {} 127.0.0.1", args.listen.port());
    server.run_on_socket(config, &socket).await?;
    Ok(())
}

fn print_ssh_config(args: PrintSshConfigArgs) {
    println!(
        "Host {}\n  HostName {}\n  Port {}\n  User webshell\n  StrictHostKeyChecking accept-new",
        args.host,
        args.listen.ip(),
        args.listen.port()
    );
}

fn enforce_listen_safety(listen: SocketAddr, allow_lan: bool) -> Result<()> {
    if allow_lan {
        warn!("--allow-lan used; local SSH bridge may expose 107 access to the network");
        return Ok(());
    }
    if is_loopback(listen.ip()) {
        Ok(())
    } else {
        bail!(
            "refusing to listen on non-loopback address {listen}; use --allow-lan only if you understand the credential exposure risk"
        )
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

fn normalize_cols(cols: u32) -> u16 {
    if cols < 20 {
        DEFAULT_COLS
    } else {
        u16::try_from(cols.min(u32::from(u16::MAX))).unwrap_or(DEFAULT_COLS)
    }
}

fn normalize_rows(rows: u32) -> u16 {
    if rows < 5 {
        DEFAULT_ROWS
    } else {
        u16::try_from(rows.min(u32::from(u16::MAX))).unwrap_or(DEFAULT_ROWS)
    }
}

fn default_host_key_path() -> Result<PathBuf> {
    let base = dirs::config_dir().context("resolve user config directory")?;
    Ok(base.join(CONFIG_DIR_NAME).join(HOST_KEY_FILE))
}

fn default_cookie_path() -> Result<PathBuf> {
    let base = dirs::config_dir().context("resolve user config directory")?;
    Ok(base.join(CONFIG_DIR_NAME).join(COOKIE_FILE))
}

fn load_or_generate_host_key(path: &PathBuf) -> Result<PrivateKey> {
    if path.exists() {
        return keys::load_secret_key(path, None)
            .with_context(|| format!("load host key {}", path.display()));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
        set_private_dir_permissions(parent)?;
    }

    let key = PrivateKey::random(&mut HostKeyRng, Algorithm::Ed25519)
        .context("generate Ed25519 SSH host key")?;
    let encoded = key
        .to_openssh(keys::ssh_key::LineEnding::LF)
        .context("encode host key as OpenSSH")?;
    std::fs::write(path, encoded.as_bytes())
        .with_context(|| format!("write host key {}", path.display()))?;
    set_private_file_permissions(path)?;
    Ok(key)
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod 700 {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

impl server::Server for SshBridgeServer {
    type Handler = Self;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self {
        self.clone()
    }

    fn handle_session_error(&mut self, error: <Self::Handler as server::Handler>::Error) {
        error!("SSH session error: {error:#}");
    }
}

impl server::Handler for SshBridgeServer {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn auth_password(
        &mut self,
        _user: &str,
        _password: &str,
    ) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _public_key: &keys::ssh_key::PublicKey,
    ) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cols = normalize_cols(col_width);
        let rows = normalize_rows(row_height);
        self.state
            .pty_sizes
            .lock()
            .await
            .insert(channel, (cols, rows));
        session.channel_success(channel)?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cols = normalize_cols(col_width);
        let rows = normalize_rows(row_height);
        self.state
            .pty_sizes
            .lock()
            .await
            .insert(channel, (cols, rows));
        if let Some(tx) = self.state.channels.lock().await.get(&channel).cloned() {
            let _ = tx.send(encode_resize_bytes(cols, rows)).await;
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let size = self.state.pty_sizes.lock().await.get(&channel).copied();
        self.start_bridge(channel, session.handle(), size).await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let command = String::from_utf8_lossy(command).to_string();
        let handle = session.handle();
        self.start_bridge(channel, handle.clone(), None).await;
        if let Some(tx) = self.state.channels.lock().await.get(&channel).cloned() {
            let _ = tx.send(format!("{command}\n").into_bytes()).await;
        }
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(tx) = self.state.channels.lock().await.get(&channel).cloned() {
            tx.send(data.to_vec())
                .await
                .context("send SSH data to WebSocket task")?;
        } else {
            debug!("dropping data for channel without active bridge: {channel:?}");
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        debug!("client sent EOF on channel {channel:?}");
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.state.channels.lock().await.remove(&channel);
        self.state.pty_sizes.lock().await.remove(&channel);
        Ok(())
    }
}

impl SshBridgeServer {
    async fn start_bridge(
        &mut self,
        channel: ChannelId,
        handle: server::Handle,
        pty_size: Option<(u16, u16)>,
    ) {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(128);
        self.state.channels.lock().await.insert(channel, tx);

        let settings = self.settings.clone();
        let state = self.state.clone();
        tokio::spawn(async move {
            let mut session_args = settings.session.clone();
            if let Some((cols, rows)) = pty_size {
                session_args.target.cols = cols;
                session_args.target.rows = rows;
            }

            let ws = match connect_shell(&session_args, &settings.cookie).await {
                Ok(ws) => ws,
                Err(err) => {
                    let msg =
                        format!("\r\n[ustc-107-ssh: failed to connect WebShell: {err:#}]\r\n");
                    let _ = handle.data(channel, Bytes::from(msg)).await;
                    let _ = handle.close(channel).await;
                    state.channels.lock().await.remove(&channel);
                    return;
                }
            };
            let (mut ws_sink, mut ws_stream) = ws.split();
            let initial_cols = session_args.target.cols;
            let initial_rows = session_args.target.rows;
            if let Err(err) = ws_sink
                .send(encode_resize_frame(initial_cols, initial_rows))
                .await
            {
                warn!("initial WebSocket resize failed: {err}");
            }
            let writer = async {
                while let Some(data) = rx.recv().await {
                    let msg = if let Some((cols, rows)) = decode_resize_bytes(&data) {
                        encode_resize_frame(cols, rows)
                    } else {
                        let text = String::from_utf8_lossy(&data).to_string();
                        encode_data_frame(&text)
                    };
                    if let Err(err) = ws_sink.send(msg).await {
                        warn!("WebSocket send failed: {err}");
                        break;
                    }
                }
                let _ = ws_sink.close().await;
            };
            let reader = async {
                while let Some(msg) = ws_stream.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Some(data) = decode_data_payload(&text) {
                                if handle.data(channel, Bytes::from(data)).await.is_err() {
                                    break;
                                }
                            } else {
                                debug!("non-data WebSocket text frame: {text}");
                            }
                        }
                        Ok(Message::Binary(bytes)) => {
                            if handle.data(channel, bytes).await.is_err() {
                                break;
                            }
                        }
                        Ok(Message::Close(frame)) => {
                            debug!("WebSocket closed: {frame:?}");
                            break;
                        }
                        Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
                        Err(err) => {
                            warn!("WebSocket read failed: {err}");
                            break;
                        }
                    }
                }
            };

            tokio::select! {
                _ = writer => {}
                _ = reader => {}
            }
            state.channels.lock().await.remove(&channel);
            state.pty_sizes.lock().await.remove(&channel);
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
    }
}

fn encode_data_frame(text: &str) -> Message {
    Message::Text(
        serde_json::json!({
            "$case": "data",
            "data": { "data": text }
        })
        .to_string()
        .into(),
    )
}

fn encode_resize_frame(cols: u16, rows: u16) -> Message {
    Message::Text(
        serde_json::json!({
            "$case": "resize",
            "resize": { "cols": cols, "rows": rows }
        })
        .to_string()
        .into(),
    )
}

fn encode_resize_bytes(cols: u16, rows: u16) -> Vec<u8> {
    format!("__USTC_107_SSH_RESIZE__:{cols}:{rows}").into_bytes()
}

fn decode_resize_bytes(data: &[u8]) -> Option<(u16, u16)> {
    let text = std::str::from_utf8(data).ok()?;
    let rest = text.strip_prefix("__USTC_107_SSH_RESIZE__:")?;
    let (cols, rows) = rest.split_once(':')?;
    Some((cols.parse().ok()?, rows.parse().ok()?))
}

fn decode_data_payload(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    match value.get("$case")?.as_str()? {
        "data" => value
            .get("data")?
            .get("data")?
            .as_str()
            .map(ToOwned::to_owned),
        "exit" | "close" | "logout" => Some(format!(
            "\n[remote session ended: {}]\n",
            value.get("$case")?.as_str()?
        )),
        _ => None,
    }
}

fn print_ws_message(msg: Message) -> Result<()> {
    match msg {
        Message::Text(text) => {
            if let Some(data) = decode_data_payload(&text) {
                print!("{data}");
            } else {
                println!("[text] {text}");
            }
        }
        Message::Binary(bytes) => println!(
            "[binary] {} byte(s): {:02x?}",
            bytes.len(),
            &bytes[..bytes.len().min(32)]
        ),
        Message::Close(frame) => println!("[close] {frame:?}"),
        Message::Ping(bytes) => println!("[ping] {} byte(s)", bytes.len()),
        Message::Pong(bytes) => println!("[pong] {} byte(s)", bytes.len()),
        Message::Frame(_) => {}
    }
    Ok(())
}

fn print_skill() {
    println!(
        r#"# ustc-107-ssh Agent Quick Skill

Use `ustc-107-ssh doctor --auth-check` then `ustc-107-ssh probe --browser-compatible --pre-read-seconds 8 --read-seconds 12 --command 'echo USTC_107_SSH_PROBE'` before assuming access works.
Never print the Cookie header. Prefer `ustc-107-ssh cookie import --cookie-stdin` or `--cookie-file`; the default per-user file is `~/.config/ustc-107-ssh/cookie.txt`.
For SSH bridge MVP, run `ustc-107-ssh serve --listen 127.0.0.1:3000`, then connect with `ssh -p 3000 127.0.0.1`.
The bridge is a localhost SSH compatibility layer over SCOW WebShell, not a real remote sshd. Shell mode is first-class; exec mode is best-effort.
Default URL: wss://107.ustc.edu.cn/api/shell?cluster=training&loginNode=11.11.10.202&path=&cols=80&rows=24&useRoot=false
"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_default_url() {
        let url = build_ws_url(&TargetArgs {
            cluster: "training".into(),
            login_node: "11.11.10.202".into(),
            path: "".into(),
            cols: 80,
            rows: 24,
            use_root: false,
        })
        .unwrap();
        assert_eq!(url.as_str(), "wss://107.ustc.edu.cn/api/shell?cluster=training&loginNode=11.11.10.202&path=&cols=80&rows=24&useRoot=false");
    }

    #[test]
    fn decodes_data_frame() {
        let payload = r#"{"$case":"data","data":{"data":"hello\n"}}"#;
        assert_eq!(decode_data_payload(payload).unwrap(), "hello\n");
    }

    #[test]
    fn formats_probe_enter_modes() {
        assert_eq!(format_probe_command("echo ok", "cr").unwrap(), "echo ok\r");
        assert_eq!(format_probe_command("echo ok", "lf").unwrap(), "echo ok\n");
        assert_eq!(
            format_probe_command("echo ok\r", "lf").unwrap(),
            "echo ok\r"
        );
        assert!(format_probe_command("echo ok", "bad").is_err());
    }

    #[test]
    fn normalizes_tiny_pty_size() {
        assert_eq!(normalize_cols(1), DEFAULT_COLS);
        assert_eq!(normalize_rows(1), DEFAULT_ROWS);
        assert_eq!(normalize_cols(120), 120);
        assert_eq!(normalize_rows(40), 40);
    }

    #[test]
    fn decodes_resize_sentinel() {
        assert_eq!(
            decode_resize_bytes(&encode_resize_bytes(100, 30)),
            Some((100, 30))
        );
    }

    #[test]
    fn cookie_summary_hides_values() {
        let summary = cookie_summary("SCOW_USER=secret; scow-dark=x");
        assert!(summary.contains("SCOW_USER"));
        assert!(summary.contains("scow-dark"));
        assert!(!summary.contains("secret"));
    }

    #[test]
    fn detects_duplicate_cookie_names_once() {
        assert_eq!(
            duplicate_cookie_names("SCOW_USER=old; _ga=a; SCOW_USER=new; _ga=b; scow-dark=x"),
            vec!["SCOW_USER".to_string(), "_ga".to_string()]
        );
    }

    #[test]
    fn rejects_non_loopback_without_allow_lan() {
        let addr: SocketAddr = "0.0.0.0:3000".parse().unwrap();
        assert!(enforce_listen_safety(addr, false).is_err());
    }

    #[test]
    fn accepts_loopback_listener() {
        let addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        assert!(enforce_listen_safety(addr, false).is_ok());
    }
}
