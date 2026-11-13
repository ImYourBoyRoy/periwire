mod accel;
mod api;
mod attach;
mod backend;
mod classify;
mod cli;
mod config;
mod daemon;
mod doctor;
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
mod tls;
mod ui;
mod urb;
#[cfg(windows)]
mod windrv;
#[cfg(windows)]
mod winusbip;

use anyhow::Result;
use backend::Backend;
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::json;

const ABOUT: &str = "Forward USB devices over the network (USB/IP inside mutual TLS). One daemon both shares and uses devices.";
const AFTER: &str = "\
QUICK START
  on the machine that has the device:   periwire daemon            (leave running; or install as a service)
                                        periwire devices            (find the busid, e.g. 2-2)
                                        periwire share 2-2
                                        periwire id                 (prints this machine's fingerprint)
  on the machine that wants the device: periwire daemon
                                        periwire trust host <host>  (compare the fingerprint, then answer y)
  and on the sharing machine:           periwire trust add <fingerprint-of-the-other-machine>
  then, on the machine that wants it:   periwire remote <host>      (see what is shared)
                                        periwire attach <host> 2-2  (the device appears natively)
                                        periwire detach 2-2         (safe eject: unmounts first)

Run `periwire doctor` when something does not work. Add --json to any command for machine-readable output.
Exit codes: 0 ok, 1 error, 2 usage, 3 daemon not running, 4 doctor found problems.";

#[derive(Parser)]
#[command(name = "periwire", version, about = ABOUT, after_help = AFTER)]
struct Cli {
    /// Machine-readable JSON output (for scripts and AI tools).
    #[arg(long, global = true)]
    json: bool,
    /// Control socket of the daemon to talk to (default: auto-detected).
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon: shares your devices, imports remote ones, serves the control socket.
    Daemon {
        /// Address the exporter listens on (default 0.0.0.0:3241).
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
        /// Do not listen for importers (use this machine only to import).
        #[arg(long)]
        no_export: bool,
        /// Share a device for this run only (busid or vid:pid); `periwire share` makes it permanent.
        #[arg(long = "share", value_name = "SPEC")]
        share: Vec<String>,
        /// Control socket path.
        #[arg(long = "daemon-socket", value_name = "PATH")]
        daemon_socket: Option<String>,
        /// Group allowed to use the control socket (when running as root).
        #[arg(long, value_name = "GROUP")]
        socket_group: Option<String>,
        /// Socket permissions in octal (default 660).
        #[arg(long, value_name = "MODE")]
        socket_mode: Option<String>,
    },
    /// Overview: daemon, what you share, what you imported, trusted machines.
    Status,
    /// List this machine's USB devices (and which are shared or in use).
    Devices,
    /// Share local devices with trusted machines. SPEC: busid (2-2) or vid:pid (1234:5678).
    Share {
        /// One or more busids or vid:pid pairs (see `periwire devices`).
        specs: Vec<String>,
        /// Write-protect the devices for everyone who imports them (USB storage only; other kinds are refused).
        #[arg(long)]
        read_only: bool,
    },
    /// Stop sharing devices. Anyone using one is disconnected and the device returns here.
    Unshare { specs: Vec<String> },
    /// List the devices a remote machine shares with you.
    #[command(alias = "list")]
    Remote {
        /// Host or host:port of the other machine (default port 3241).
        addr: String,
    },
    /// Import a remote device here. It appears as if it were plugged into this machine.
    Attach {
        /// Host or host:port of the machine the device is plugged into.
        addr: String,
        /// The device's port id on that machine (see `periwire remote <addr>`), e.g. 2-2.
        busid: String,
        /// Remember it: re-attach automatically after reboots and network drops.
        #[arg(long)]
        auto: bool,
        /// Label for the remembered connection.
        #[arg(long)]
        name: Option<String>,
        /// Write-protect the device while it is attached (USB storage). Default is read and write.
        #[arg(long)]
        read_only: bool,
    },
    /// Safely eject an imported device: flush, unmount, detach, hand it back.
    /// TARGET: attachment number, busid, vid:pid, or busid@host.
    #[command(alias = "eject")]
    Detach {
        target: String,
        /// Detach even if files are open (busy mounts are detached lazily; unsaved data may be lost).
        #[arg(long)]
        force: bool,
    },
    /// Force-disconnect a peer that is using one of your shared devices (it comes back to you at once).
    /// TARGET: session number or busid.
    Kick { target: String },
    /// Forget a remembered connection (see `status`).
    Forget { addr: String, busid: String },
    /// Manage which machines may connect.
    #[command(subcommand)]
    Trust(TrustCmd),
    /// Start or stop the exporter (listening for importers).
    Export {
        #[arg(value_parser = ["on", "off", "status"])]
        state: String,
    },
    /// Give a local device a name you recognise (peers see it too). Use `--clear` to remove it.
    Label {
        /// busid or vid:pid of a local device.
        device: String,
        /// The name, e.g. "Sam's iPad".
        name: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Print this machine's fingerprint (give it to machines that should trust you).
    Id,
    /// Identity management.
    #[command(subcommand)]
    Identity(IdentityCmd),
    /// Check that everything needed works, and say how to fix what doesn't.
    Doctor,
    /// Show the daemon log.
    Logs {
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Keep following new lines.
        #[arg(short, long)]
        follow: bool,
    },
    /// Stream daemon events as they happen.
    Watch,
    /// Ask the running daemon to stop (imports are ejected, shared devices go back to the host).
    Stop,
    /// Run the MCP server on stdio so an AI assistant can control periwire.
    Mcp,
    /// List the control API methods, or call one: `periwire api status`, `periwire api driver.status`, `periwire api label '{"key":"..","label":".."}'`.
    Api {
        /// Method to call on the running daemon (omit to list them).
        method: Option<String>,
        /// JSON parameters (default {}).
        params: Option<String>,
    },
    /// Print or install the system service for the daemon.
    #[command(subcommand)]
    Service(ServiceCmd),
    /// Manage the Windows virtual USB host controller driver that PeriWire carries inside this program.
    #[command(subcommand)]
    Driver(DriverCmd),
    /// Print shell completions.
    Completions {
        #[arg(value_parser = ["bash", "zsh", "fish", "powershell", "elvish"])]
        shell: String,
    },
    #[command(hide = true)]
    MacProbe,
    #[command(hide = true)]
    Urbtest { addr: String, busid: String },
}

#[derive(Subcommand)]
enum TrustCmd {
    /// List trusted machines.
    List,
    /// Trust a machine by the fingerprint it printed with `periwire id`.
    Add {
        /// 64 hex characters, as printed by `periwire id` on the other machine.
        fingerprint: String,
        /// Friendly name shown in status.
        #[arg(long, default_value = "peer")]
        name: String,
    },
    /// Stop trusting a machine.
    Remove { fingerprint: String },
    /// Connect to a machine, show its fingerprint and (after you confirm) trust it.
    Host {
        /// Host or host:port of the other machine.
        addr: String,
        /// Skip the confirmation (you compared the fingerprint out of band).
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum IdentityCmd {
    /// Create a new certificate/fingerprint. Every computer that trusted this one must trust the new fingerprint.
    Reset {
        /// Confirm: peers will stop trusting this computer until they re-pair.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum DriverCmd {
    /// Is the driver installed? (Only Windows needs one.)
    Status,
    /// Install the signed driver carried inside this program (Windows; needs Administrator).
    Install {
        /// Ask Windows for administrator rights (UAC prompt) if this shell is not elevated.
        #[arg(long)]
        elevate: bool,
    },
    /// Remove the driver, its device and the extracted tools.
    Uninstall {
        /// Ask Windows for administrator rights (UAC prompt) if this shell is not elevated.
        #[arg(long)]
        elevate: bool,
    },
    /// Write the driver packages and tools to a folder (manual or enterprise installs).
    Extract { dir: std::path::PathBuf },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Print the service definition (systemd unit / launchd plist).
    Print {
        /// systemd user unit instead of the system unit.
        #[arg(long)]
        user: bool,
    },
    /// Install and start the system service (Linux, needs root).
    Install,
    /// Stop and remove the system service (Linux, needs root).
    Uninstall,
}

pub struct RunOpts {
    pub listen: Option<String>,
    pub no_export: bool,
    pub share: Vec<String>,
    pub socket: Option<String>,
    pub socket_group: Option<String>,
    pub socket_mode: Option<u32>,
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
        Cmd::Daemon { listen, no_export, share, daemon_socket, socket_group, socket_mode } => {
            let socket_mode = match socket_mode {
                Some(m) => Some(u32::from_str_radix(&m, 8).map_err(|_| anyhow::anyhow!("--socket-mode must be octal, like 660"))?),
                None => None,
            };
            daemon::run(RunOpts { listen, no_export, share, socket: daemon_socket.or(cli.socket), socket_group, socket_mode }).await?;
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

#[cfg(not(windows))]
fn driver_cmd(_: DriverCmd, _json: bool) -> Result<i32> {
    println!("Only Windows needs a separate driver. Linux uses the kernel's vhci-hcd module and macOS uses Apple's IOUSBHostControllerInterface.");
    Ok(0)
}

#[cfg(windows)]
fn driver_cmd(dc: DriverCmd, json: bool) -> Result<i32> {
    use crate::windrv;
    let print_status = |st: &windrv::Status| {
        if json {
            cli::print_json(&serde_json::to_value(st).unwrap_or_default());
            return;
        }
        println!("{} {}", ui::bold("Windows virtual USB host controller"), format!("(usbip-win2 {}, carried inside this program)", st.embedded_version));
        let pk = if st.packages.is_empty() {
            "not installed".to_string()
        } else {
            st.packages.iter().map(|p| format!("{} ({}, {})", p.original.trim_end_matches(".inf"), p.published, p.version)).collect::<Vec<_>>().join(", ")
        };
        println!("  driver packages  {pk}");
        println!("  host controller  {}", st.device.clone().unwrap_or_else(|| "not present".into()));
        println!("  tools            {}", st.tools.clone().unwrap_or_else(|| "not extracted yet".into()));
        println!("  C++ runtime      {}", if st.vc_runtime { "ok" } else { "missing: install the Microsoft Visual C++ 2015-2022 x64 runtime (winget install Microsoft.VCRedist.2015+.x64)" });
        println!("  this shell       {}", if st.admin { "elevated (Administrator)" } else { "not elevated" });
        println!("  ready to import  {}", if st.ready { "yes" } else { "no: run `periwire driver install` as Administrator" });
    };
    match dc {
        DriverCmd::Status => {
            let st = windrv::status();
            print_status(&st);
            Ok(if st.ready { 0 } else { 1 })
        }
        DriverCmd::Install { elevate } => {
            if !windrv::is_admin() && elevate {
                eprintln!("asking Windows for administrator rights...");
                let code = windrv::elevate_and_wait(&["driver", "install"])?;
                let st = windrv::status();
                print_status(&st);
                return Ok(if st.ready {
                    0
                } else if code != 0 {
                    code
                } else {
                    1
                });
            }
            let reboot = windrv::install()?;
            print_status(&windrv::status());
            if reboot {
                println!("Windows asked for a restart to finish installing the driver.");
            }
            Ok(0)
        }
        DriverCmd::Uninstall { elevate } => {
            if !windrv::is_admin() && elevate {
                let code = windrv::elevate_and_wait(&["driver", "uninstall"])?;
                print_status(&windrv::status());
                return Ok(code);
            }
            windrv::uninstall()?;
            println!("removed the virtual host controller, both driver packages and the extracted tools");
            Ok(0)
        }
        DriverCmd::Extract { dir } => {
            windrv::extract(&dir)?;
            println!("wrote the usbip-win2 {} driver packages and tools to {}", windrv::VERSION, dir.display());
            println!("install by hand with: pnputil /add-driver usbip2_filter.inf /install  then  devnode.exe install usbip2_ude.inf {}", windrv::HWID);
            Ok(0)
        }
    }
}

fn sh(cmd: &str, args: &[&str]) -> Result<()> {
    let st = std::process::Command::new(cmd).args(args).status()?;
    if !st.success() {
        anyhow::bail!("`{cmd} {}` failed", args.join(" "));
    }
    Ok(())
}

fn service_install() -> Result<i32> {
    if cfg!(windows) {
        // A console program is not a native Windows service; run it elevated at logon through Task Scheduler.
        let exe = std::env::current_exe()?;
        sh("schtasks", &["/Create", "/F", "/SC", "ONLOGON", "/RL", "HIGHEST", "/TN", "PeriWire", "/TR", &format!("\"{}\" daemon", exe.display())])?;
        let _ = sh("schtasks", &["/Run", "/TN", "PeriWire"]);
        println!("installed the PeriWire task (runs at logon with highest privileges) and started it");
        return Ok(0);
    }
    if !cfg!(target_os = "linux") {
        anyhow::bail!("automatic install is Linux-only for now; use `periwire service print` and install it yourself");
    }
    if !daemon::is_root() {
        anyhow::bail!("run with sudo");
    }
    std::fs::write("/etc/systemd/system/periwire.service", cli::service_text(false))?;
    let _ = sh("groupadd", &["-f", "periwire"]);
    sh("systemctl", &["daemon-reload"])?;
    sh("systemctl", &["enable", "--now", "periwire.service"])?;
    println!("installed and started periwire.service. Let your user control it: sudo usermod -aG periwire $USER (then log in again)");
    Ok(0)
}

fn service_uninstall() -> Result<i32> {
    if cfg!(windows) {
        let _ = sh("schtasks", &["/End", "/TN", "PeriWire"]);
        sh("schtasks", &["/Delete", "/F", "/TN", "PeriWire"])?;
        println!("removed the PeriWire task");
        return Ok(0);
    }
    if !daemon::is_root() {
        anyhow::bail!("run with sudo");
    }
    let _ = sh("systemctl", &["disable", "--now", "periwire.service"]);
    let _ = std::fs::remove_file("/etc/systemd/system/periwire.service");
    let _ = sh("systemctl", &["daemon-reload"]);
    println!("removed periwire.service");
    Ok(0)
}

#[cfg(test)]
mod help_tests {
    use super::*;
    use clap::Command;
    const HELP: &str = include_str!("../../../gui/ui/help.js");

    fn longs(c: &Command) -> Vec<String> {
        c.get_arguments().filter_map(|a| a.get_long().map(String::from)).collect()
    }
    fn shorts(c: &Command) -> Vec<char> {
        c.get_arguments().filter_map(|a| a.get_short()).collect()
    }

    /// Every `periwire ...` command and flag written in the in-app help must exist, so the help cannot go stale unnoticed.
    #[test]
    fn help_matches_cli() {
        let root = Cli::command();
        let mut seen = 0;
        let mut rest = HELP;
        while let Some(i) = rest.find("periwire ") {
            let before = rest[..i].chars().last().unwrap_or(' ');
            let tail = &rest[i + "periwire ".len()..];
            rest = tail;
            if !matches!(before, '"' | '`' | '\'') {
                continue; // prose, not a command
            }
            let end = tail.find(['"', '`', '\'']).unwrap_or(tail.len());
            let words: Vec<&str> = tail[..end].split_whitespace().collect();
            let Some(&first) = words.first() else { continue };
            if first.starts_with('-') || !first.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                continue;
            }
            seen += 1;
            let sub = root.find_subcommand(first).unwrap_or_else(|| panic!("help.js mentions `periwire {first}` but the CLI has no such command"));
            // a second word that is lowercase and not an option is a nested subcommand when the command has any
            let nested = if sub.has_subcommands() { words.get(1).and_then(|w| sub.find_subcommand(w)) } else { None };
            if sub.has_subcommands() {
                let w = words.get(1).copied().unwrap_or("");
                assert!(w.starts_with('-') || nested.is_some(), "help.js: `periwire {first} {w}`: `{w}` is not a subcommand of `{first}`");
            }
            let (mut l, mut s) = (longs(&root), shorts(&root));
            l.extend(longs(sub));
            s.extend(shorts(sub));
            if let Some(n) = nested {
                l.extend(longs(n));
                s.extend(shorts(n));
            }
            for w in &words {
                if let Some(f) = w.strip_prefix("--") {
                    assert!(l.iter().any(|x| x == f), "help.js: `periwire {}`: unknown flag --{f}", words.join(" "));
                } else if let Some(f) = w.strip_prefix('-') {
                    for c in f.chars() {
                        assert!(s.contains(&c), "help.js: `periwire {}`: unknown flag -{c}", words.join(" "));
                    }
                }
            }
        }
        assert!(seen >= 15, "only found {seen} commands in help.js; the checker may be broken");
    }
}
