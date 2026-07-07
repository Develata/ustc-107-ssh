use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{COOKIE, ORIGIN, USER_AGENT};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};
use url::Url;

const DEFAULT_CLUSTER: &str = "training";
const DEFAULT_LOGIN_NODE: &str = "11.11.10.202";
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_ORIGIN: &str = "https://107.ustc.edu.cn";
const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (compatible; ustc-107-ssh/0.1; +https://github.com/Develata/ustc-107-ssh)";
const ENV_COOKIE: &str = "USTC_107_COOKIE";

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
    /// Check local assumptions and cookie source presence.
    Doctor(SessionArgs),
    /// Connect to the WebSocket and send one probe command.
    Probe(ProbeArgs),
    /// Pipe local stdin/stdout to the WebSocket data stream.
    Attach(SessionArgs),
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
        Command::Doctor(args) => doctor(args)?,
        Command::Probe(args) => probe(args).await?,
        Command::Attach(args) => attach(args).await?,
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
    if sources != 1 {
        bail!(
            "provide exactly one cookie source: --cookie, --cookie-file, --cookie-stdin, or env {ENV_COOKIE}"
        );
    }

    let cookie = if let Some(value) = &args.cookie {
        value.clone()
    } else if let Some(path) = &args.cookie_file {
        std::fs::read_to_string(path)
            .with_context(|| format!("read cookie file {}", path.display()))?
    } else {
        rpassword::prompt_password("Cookie header: ")?
    };

    let cookie = cookie.trim().to_string();
    if cookie.is_empty() {
        bail!("cookie is empty");
    }
    Ok(cookie)
}

fn cookie_summary(cookie: &str) -> String {
    let names: Vec<&str> = cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty())
        .collect();
    format!(
        "{} cookie pair(s), {} byte(s), names={:?}",
        names.len(),
        cookie.len(),
        names
    )
}

fn doctor(args: SessionArgs) -> Result<()> {
    if args.insecure_tls {
        bail!("--insecure-tls is intentionally not supported in this MVP");
    }
    let url = build_ws_url(&args.target)?;
    println!("os: {}", std::env::consts::OS);
    println!("arch: {}", std::env::consts::ARCH);
    println!("websocket_url: {url}");
    println!("origin: {}", args.origin);
    println!("tls_verification: enabled");
    match resolve_cookie(&args) {
        Ok(cookie) => println!("cookie: present ({})", cookie_summary(&cookie)),
        Err(err) => println!("cookie: missing or invalid ({err})"),
    }
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

async fn probe(args: ProbeArgs) -> Result<()> {
    let cookie = resolve_cookie(&args.session)?;
    eprintln!("cookie: {}", cookie_summary(&cookie));
    let mut ws = connect_shell(&args.session, &cookie).await?;

    let command = if args.command.ends_with('\n') {
        args.command.clone()
    } else {
        format!("{}\n", args.command)
    };
    ws.send(encode_data_frame(&command))
        .await
        .context("send probe command")?;
    eprintln!("sent command: {}", args.command);

    let deadline = tokio::time::sleep(Duration::from_secs(args.read_seconds));
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
    let _ = ws.close(None).await;
    Ok(())
}

async fn attach(args: SessionArgs) -> Result<()> {
    let cookie = resolve_cookie(&args)?;
    eprintln!("cookie: {}", cookie_summary(&cookie));
    let ws = connect_shell(&args, &cookie).await?;
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

Use `ustc-107-ssh doctor` then `ustc-107-ssh probe --command 'echo USTC_107_SSH_PROBE'` before assuming access works.
Never print or store the Cookie header. Prefer `USTC_107_COOKIE`, `--cookie-file`, or `--cookie-stdin`.
Current commands are protocol probe/attach only; do not claim full SSH bridge support until `serve` exists.
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
    fn cookie_summary_hides_values() {
        let summary = cookie_summary("SCOW_USER=secret; scow-dark=x");
        assert!(summary.contains("SCOW_USER"));
        assert!(summary.contains("scow-dark"));
        assert!(!summary.contains("secret"));
    }
}
