mod accel;
mod api;
mod args;
mod attach;
mod backend;
mod classify;
mod cli;
mod config;
mod daemon;
mod doctor;
mod driver_cli;
mod events;
mod export;
mod guard;
mod hci;
mod identity;
mod import;
#[cfg(target_os = "macos")]
mod mac;
mod mcp;
mod ntfs;
mod resume;
mod selfinstall;
mod selfupdate;
mod service;
mod tls;
mod tray;
mod ui;
mod update;
mod urb;
#[cfg(windows)]
mod windrv;
#[cfg(windows)]
mod winusbip;

use anyhow::Result;
use args::{Cli, Cmd, IdentityCmd, ServiceCmd, TrustCmd};
use backend::Backend;
use clap::{CommandFactory, Parser};
use driver_cli::driver_cmd;
use serde_json::json;
use service::{service_install, service_uninstall};

pub struct RunOpts {
    pub listen: Option<String>,
    pub no_export: bool,
    pub share: Vec<String>,
    pub socket: Option<String>,
    pub socket_group: Option<String>,
    pub socket_mode: Option<u32>,
    pub no_tray: bool,
}

/// Error that carries a process exit code.
#[derive(Debug)]
struct Exit(i32, String);
impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.1)
    }
}
impl std::error::Error for Exit {}

async fn open(need_daemon: bool) -> Result<Backend> {
    Backend::open(need_daemon).await.map_err(|e| {
        let code = if e.code == "not_running" { 3 } else { 1 };
        Exit(code, e.message).into()
    })
}

#[tokio::main]
async fn main() {
    selfupdate::cleanup_old(); // Windows: a previous self-update leaves the old program beside this one
    let cli = Cli::parse();
    if let Some(sock) = &cli.socket {
        std::env::set_var("PERIWIRE_SOCKET", sock);
    }
    let json_out = cli.json;
    let code = match run(cli).await {
        Ok(c) => c,
        Err(e) => {
            let (code, msg) = match e.downcast_ref::<Exit>() {
                Some(x) => (x.0, x.1.clone()),
                None => (1, format!("{e:#}")),
            };
            if json_out {
                println!("{}", json!({"error": {"message": msg}}));
            } else {
                eprintln!("{} {msg}", ui::red("error:"));
            }
            code
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<i32> {
    let j = cli.json;
    match cli.cmd {
        Cmd::Daemon { listen, no_export, share, daemon_socket, socket_group, socket_mode, no_tray } => {
            let socket_mode = match socket_mode {
                Some(m) => Some(u32::from_str_radix(&m, 8).map_err(|_| anyhow::anyhow!("--socket-mode must be octal, like 660"))?),
                None => None,
            };
            daemon::run(RunOpts { listen, no_export, share, socket: daemon_socket.or(cli.socket), socket_group, socket_mode, no_tray }).await?;
            Ok(0)
        }
        Cmd::Status => cli::status(&mut open(false).await?, j).await,
        Cmd::Devices => cli::devices(&mut open(false).await?, j).await,
        Cmd::Share { specs, read_only } => {
            let mut b = open(false).await?;
            for sp in &specs {
                cli::simple(&mut b, j, "share.add", json!({"spec": sp, "read_only": read_only}), |_| format!("sharing {sp}{}", if read_only { "  (read-only for everyone)" } else { "" })).await?;
            }
            if !b.has_daemon() && !j {
                eprintln!("{}", ui::yellow("the daemon is not running: this takes effect when it starts (`periwire daemon`)"));
            }
            Ok(0)
        }
        Cmd::Unshare { specs } => {
            let mut b = open(false).await?;
            for sp in &specs {
                cli::simple(&mut b, j, "share.remove", json!({"spec": sp}), |v| {
                    format!(
                        "stopped sharing {sp}{}",
                        match v["disconnected"].as_u64() {
                            Some(k) if k > 0 => format!(" ({k} peer(s) disconnected)"),
                            _ => String::new(),
                        }
                    )
                })
                .await?;
            }
            Ok(0)
        }
        Cmd::Remote { addr } => cli::remote(&mut open(false).await?, &addr, j).await,
        Cmd::Attach { addr, busid, auto, name, read_only } => {
            let mut b = open(true).await?;
            cli::simple(&mut b, j, "attach", json!({"addr": addr, "busid": busid, "auto": auto, "name": name, "read_only": read_only}), |v| {
                format!(
                    "{} {} {}:{} from {}{}",
                    ui::green("attached"),
                    v["busid"].as_str().unwrap_or(""),
                    v["vid"].as_str().unwrap_or(""),
                    v["pid"].as_str().unwrap_or(""),
                    v["addr"].as_str().unwrap_or(""),
                    if auto { "  (will re-attach automatically)" } else { "" }
                )
            })
            .await
        }
        Cmd::Detach { target, force } => {
            let mut b = open(true).await?;
            let a = cli::resolve_attachment(&mut b, &target).await?;
            cli::simple(&mut b, j, "detach", json!({"id": a["id"], "force": force}), |v| {
                let um: Vec<&str> = v["unmounted"].as_array().map(|x| x.iter().filter_map(|m| m.as_str()).collect()).unwrap_or_default();
                format!("{} {}{}", ui::green("ejected"), a["busid"].as_str().unwrap_or(""), if um.is_empty() { String::new() } else { format!("  (unmounted {})", um.join(", ")) })
            })
            .await
        }
        Cmd::Kick { target } => {
            let mut b = open(true).await?;
            let se = cli::resolve_session(&mut b, &target).await?;
            cli::simple(&mut b, j, "kick", json!({"session": se["id"]}), |_| format!("disconnected {}; the device is back on this machine", se["busid"].as_str().unwrap_or(""))).await
        }
        Cmd::Forget { addr, busid } => {
            let mut b = open(false).await?;
            cli::simple(&mut b, j, "connection.remove", json!({"addr": addr, "busid": busid}), |_| "forgotten".into()).await
        }
        Cmd::Trust(t) => {
            let mut b = open(false).await?;
            match t {
                TrustCmd::List => {
                    cli::simple(&mut b, j, "trust.list", json!({}), |v| {
                        let a = v.as_array().unwrap();
                        if a.is_empty() {
                            return "no trusted machines".into();
                        }
                        a.iter().map(|t| format!("{}  {}", t["fingerprint"].as_str().unwrap_or(""), t["name"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n")
                    })
                    .await
                }
                TrustCmd::Add { fingerprint, name } => {
                    cli::simple(&mut b, j, "trust.add", json!({"fingerprint": fingerprint, "name": name}), |_| format!("{} {}", ui::green("trusted"), ui::short_fp(&fingerprint))).await
                }
                TrustCmd::Remove { fingerprint } => cli::simple(&mut b, j, "trust.remove", json!({"fingerprint": fingerprint}), |_| "removed".into()).await,
                TrustCmd::Host { addr, yes } => cli::trust_host(&mut b, &addr, yes, j).await,
            }
        }
        Cmd::Export { state } => {
            let mut b = open(true).await?;
            match state.as_str() {
                "status" => cli::status(&mut b, j).await,
                s => cli::simple(&mut b, j, "export.set", json!({"enabled": s == "on"}), |_| format!("exporter {s}")).await,
            }
        }
        Cmd::Label { device, name, clear } => {
            let mut b = open(false).await?;
            if !clear && name.is_none() {
                anyhow::bail!("give a name, or --clear");
            }
            let key = cli::device_key(&mut b, &device).await?;
            let label = if clear { String::new() } else { name.unwrap_or_default() };
            cli::simple(&mut b, j, "device.label", json!({"key": key, "label": label}), |_| if clear { "label cleared".into() } else { format!("{device} is now called {label}") }).await
        }
        Cmd::Identity(IdentityCmd::Reset { yes }) => {
            if !yes && !j {
                anyhow::bail!("this gives the computer a new fingerprint and every peer will stop trusting it until they re-pair; re-run with --yes to do it");
            }
            cli::simple(&mut open(false).await?, j, "identity.reset", json!({"confirm": yes}), |v| {
                format!("new fingerprint: {}\nTell every computer that trusted this one to run `periwire trust add` with it.", v["fingerprint"].as_str().unwrap_or(""))
            })
            .await
        }
        Cmd::Id => cli::simple(&mut open(false).await?, j, "identity", json!({}), |v| v["fingerprint"].as_str().unwrap_or("").into()).await,
        Cmd::Doctor => cli::doctor(&mut open(false).await?, j).await,
        Cmd::SelfInstall { yes, dry_run, no_shell, quiet } => selfinstall::self_install(yes, dry_run, no_shell, quiet, j),
        Cmd::SelfUninstall { yes } => selfinstall::self_uninstall(yes, j),
        Cmd::SelfUpdate { yes, dry_run, to, force } => tokio::task::spawn_blocking(move || selfupdate::self_update(yes, dry_run, to, force, j)).await?,
        Cmd::Update => cli::update(j).await,
        Cmd::Logs { lines, follow } => cli::logs(lines, follow).await,
        Cmd::Watch => cli::watch(j).await,
        Cmd::Stop => cli::simple(&mut open(true).await?, j, "shutdown", json!({}), |_| "daemon is stopping".into()).await,
        Cmd::Mcp => {
            mcp::serve().await?;
            Ok(0)
        }
        Cmd::Api { method: Some(m), params } => {
            let p: serde_json::Value = match params {
                Some(t) => serde_json::from_str(&t).map_err(|e| anyhow::anyhow!("params must be JSON: {e}"))?,
                None => json!({}),
            };
            let v = cli::call(&mut open(true).await?, &m, p).await?;
            cli::print_json(&v);
            Ok(0)
        }
        Cmd::Api { method: None, .. } => {
            let v = api::METHODS.iter().map(|(m, h)| json!({"method": m, "params": h})).collect::<Vec<_>>();
            if j {
                cli::print_json(&json!(v));
            } else {
                for (m, h) in api::METHODS {
                    println!("{:<18} {h}", ui::bold(m));
                }
            }
            Ok(0)
        }
        Cmd::Service(sc) => match sc {
            ServiceCmd::Print { user } => {
                print!("{}", cli::service_text(user));
                Ok(0)
            }
            ServiceCmd::Install => service_install(),
            ServiceCmd::Uninstall => service_uninstall(),
        },
        Cmd::Driver(dc) => driver_cmd(dc, j),
        Cmd::Completions { shell } => {
            use clap_complete::{generate, Shell};
            let sh = match shell.as_str() {
                "bash" => Shell::Bash,
                "zsh" => Shell::Zsh,
                "fish" => Shell::Fish,
                "powershell" => Shell::PowerShell,
                _ => Shell::Elvish,
            };
            generate(sh, &mut Cli::command(), "periwire", &mut std::io::stdout());
            Ok(0)
        }
        Cmd::MacProbe => {
            #[cfg(target_os = "macos")]
            match mac::probe() {
                Ok(()) => println!("OK: this process may create a virtual USB host controller"),
                Err(e) => println!("REFUSED: {e}"),
            }
            #[cfg(not(target_os = "macos"))]
            println!("mac-probe only applies to macOS");
            Ok(0)
        }
        Cmd::Urbtest { addr, busid } => {
            let c = urb::UrbClient::import(&addr, &busid, false, false).await?;
            let d = urb::read_device_descriptor(&c).await?;
            println!("device descriptor ({} bytes): {}", d.len(), d.iter().map(|b| format!("{b:02x}")).collect::<String>());
            Ok(0)
        }
    }
}
