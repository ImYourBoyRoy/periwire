//! The command line: every command and option, as `clap` definitions.
use clap::{Parser, Subcommand};

pub(crate) const ABOUT: &str = "Forward USB devices over the network (USB/IP inside mutual TLS). One daemon both shares and uses devices.";
pub(crate) const AFTER: &str = "\
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
pub(crate) struct Cli {
    /// Machine-readable JSON output (for scripts and AI tools).
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Control socket of the daemon to talk to (default: auto-detected).
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) socket: Option<String>,
    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
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
        /// Do not start the taskbar icon (the desktop app's tray) alongside the daemon.
        #[arg(long)]
        no_tray: bool,
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
    /// Make `periwire` work in every shell: a link in ~/.local/bin and a block in your shell profiles (Linux, macOS), or your user PATH (Windows). Reversible.
    SelfInstall {
        /// Do not ask first.
        #[arg(short, long)]
        yes: bool,
        /// Only show what would change.
        #[arg(long)]
        dry_run: bool,
        /// Skip the shell profiles (Linux, macOS): only link the command.
        #[arg(long)]
        no_shell: bool,
        /// Print nothing but errors.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Undo `self-install`: remove the link, the profile blocks and the PATH entry it made (the program, your identity and settings stay).
    SelfUninstall {
        /// Do not ask first.
        #[arg(short, long)]
        yes: bool,
    },
    /// Update this copy of PeriWire to the latest release (or --to VERSION): downloads the release archive, checks its SHA-256, replaces the programs beside this one.
    SelfUpdate {
        /// Do not ask first.
        #[arg(short, long)]
        yes: bool,
        /// Only show what would happen.
        #[arg(long)]
        dry_run: bool,
        /// Install this version instead of the latest (also to go back).
        #[arg(long, value_name = "VERSION")]
        to: Option<String>,
        /// Reinstall even if this is already the latest version.
        #[arg(long)]
        force: bool,
    },
    /// Check whether a newer PeriWire has been released (one request to GitHub; installs nothing).
    Update,
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
pub(crate) enum TrustCmd {
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
pub(crate) enum IdentityCmd {
    /// Create a new certificate/fingerprint. Every computer that trusted this one must trust the new fingerprint.
    Reset {
        /// Confirm: peers will stop trusting this computer until they re-pair.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum DriverCmd {
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
pub(crate) enum ServiceCmd {
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

#[cfg(test)]
mod help_tests {
    use super::*;
    use clap::{Command, CommandFactory};
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
