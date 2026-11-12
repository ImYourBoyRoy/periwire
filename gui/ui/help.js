// All in-app help lives here: plain language first, expert detail folded away.
// A Rust test (help_matches_cli in crates/periwire/src/main.rs) checks that every `periwire ...` command and --flag
// written in this file exists, so the help cannot silently go stale when the CLI changes. Backticks mean "code".

export const PAGES = {
  overview: {
    title: "Overview",
    what: "A live picture of this computer: what it shares, what it uses from other computers, and how much data is moving.",
    steps: [
      "The middle box is this computer. Computers on the left use your devices; computers on the right have devices you use.",
      "Click a connection below to see its details, or to eject it safely.",
      "Nothing is shared until you switch a device on in Share.",
    ],
    expert: "The tiles and diagram come from the daemon's status snapshot (`periwire status`) and update on every event (`periwire watch`). ↓ is received, ↑ is sent; rates are averaged over a few seconds.",
  },
  share: {
    title: "Share",
    what: "The USB devices plugged into this computer. Switch one on to let trusted computers use it.",
    steps: [
      "Keep 'Let other computers use my devices' on (it is on by default).",
      "Switch a device on. It stays usable here until another computer takes it; while they use it, it is gone from this computer.",
      "'Read-only for others' (USB drives) gives every computer that connects a write-protected drive.",
      "The pencil renames a device. The new name shows on the other computers too.",
    ],
    expert: "A share is a port id (`2-2`) or a model (`1234:5678`). `periwire share 2-2 --read-only` and `periwire devices` do the same from a terminal. Only computers you trust can connect, over TCP port 3241 inside mutual TLS.",
  },
  connect: {
    title: "Connect",
    what: "Use a device that is plugged into another computer, as if it were plugged into this one.",
    steps: [
      "Type the other computer's name or address and press Browse. The two computers must trust each other first (see Trust).",
      "Press Attach on a device. It appears here like any USB device.",
      "'Reconnect automatically' keeps it attached after reboots and network drops. 'Read-only' write-protects a USB drive.",
      "Always press Eject when you are done: it saves and unmounts the drive first.",
    ],
    expert: "`periwire remote HOST`, `periwire attach HOST 2-2 --auto --read-only` and `periwire detach 2-2` do the same. Linux needs root and the vhci-hcd module; macOS needs Apple's host-controller entitlement (or SIP relaxed for testing); Windows needs the USB driver, which the banner installs.",
  },
  trust: {
    title: "Trust",
    what: "Only computers you trust can connect to each other. Trust is a short code, a fingerprint, that identifies one computer.",
    steps: [
      "On the other computer open Trust and read its code (or run `periwire id` there).",
      "Here, add that computer and check that the code matches exactly. Compare it on both screens.",
      "If a computer's code ever changes unexpectedly, do not trust it: ask its owner why.",
      "'Reset fingerprint' gives this computer a new identity; every other computer must trust it again.",
    ],
    expert: "Identity is a self-signed certificate; each side pins the other's SHA-256 fingerprint (no certificate authority, no passwords). `periwire trust host ADDR` shows what a host presents without trusting it; `periwire trust add FINGERPRINT --name NAME` trusts it.",
  },
  health: {
    title: "Health",
    what: "Checks that everything needed is working, with a fix for anything that is not.",
    steps: [
      "Green is fine. Amber means something limits what you can do; the fix is shown underneath.",
      "Run the checks again after you change something.",
      "On Windows the USB driver check has an Install button.",
    ],
    expert: "`periwire doctor` prints the same list. The checks cover USB access, importer support (vhci-hcd, the Apple entitlement or the Windows driver), the exporter, trust, and conflicting software such as VirtualHere.",
  },
  logs: {
    title: "Activity",
    what: "What the daemon is doing right now: who connected, what was attached or refused, and why.",
    steps: [
      "Lines about errors or refusals are highlighted.",
      "When something fails, the reason is usually here.",
    ],
    expert: "`periwire logs -f` follows the same stream. Start the daemon with the environment variable `PERIWIRE_TRACE=1` to log every USB request.",
  },
};

export const BASICS = {
  intro: "PeriWire makes a USB device that is plugged into one computer appear on another, as if it were plugged in there.",
  steps: [
    ["On the computer with the device", "Open Share and switch the device on."],
    ["Pair the two computers", "Each one adds the other under Trust, after you check that the codes match."],
    ["On the other computer", "Open Connect, type the first computer's name, press Browse, then Attach."],
  ],
  tips: [
    "Eject a drive before you unplug it or turn off either computer.",
    "Read-only protects a drive from being changed. Use it when you only want to look.",
    "Speed depends on the network between the two computers. Wired or good Wi-Fi feels like a local drive; the internet is slower.",
  ],
};

export const GLOSSARY = [
  ["Share", "Letting trusted computers use one of this computer's USB devices."],
  ["Attach", "Using another computer's device here. Also called import or connect."],
  ["Eject", "The safe way to stop using a device: files are saved and the drive is unmounted before it is handed back."],
  ["Disconnect", "Taking a device back from a computer that is using it. It is immediate, so that computer can lose unsaved data."],
  ["Trust", "Telling this computer that another computer is allowed to connect."],
  ["Fingerprint", "A short code that identifies one computer. Compare it on both screens before you trust."],
  ["Read-only", "Write protection for USB drives: nothing on the drive can be changed through PeriWire."],
  ["Port id", "Where a device is plugged in on its computer, like 2-2. It stays the same if you re-plug into the same port."],
  ["Daemon", "The background program that does the work. The app, the command line and AI tools all talk to it."],
  ["USB driver (Windows)", "Windows needs a small virtual USB driver to use remote devices. PeriWire carries it and installs it with one click."],
  ["Reconnect automatically", "Attach a device again by itself after a reboot or a network drop."],
];

export const EXPERT = {
  sections: [
    ["How it works", "Transport is TCP port 3241. The protocol is USB/IP inside TLS 1.3 with mutual certificate authentication; each side pins the other's fingerprint (no CA). A private extension carries device names and session options such as read-only. Exporting uses a built-in libusb; importing uses the kernel's vhci-hcd (Linux), IOUSBHostControllerInterface (macOS) or the embedded usbip-win2 driver (Windows)."],
    ["Files and sockets", "Identity and settings live in the PeriWire home directory (`PERIWIRE_HOME`). The daemon's control socket is /run/periwire/periwire.sock as root, $XDG_RUNTIME_DIR/periwire.sock for a user daemon, and the named pipe \\\\.\\pipe\\periwire on Windows (`PERIWIRE_SOCKET` overrides)."],
    ["Speed", "USB storage costs about three network round trips per command, so speed is limited by round-trip time on slow or distant links and by bandwidth on large reads. Expect several MB/s on Wi-Fi; across the internet it is capped by the sender's upload speed."],
    ["Read-only", "The computer holding the drive reports it write-protected (the write-protect bit in MODE SENSE) and refuses every SCSI command that is not a known read command. It works for USB storage; other devices are refused in read-only mode instead of being left writable."],
    ["Not supported yet", "Isochronous devices (cameras, microphones), an internet relay, and UAS drives on macOS."],
    ["Automation", "`periwire --json` prints JSON for any command; `periwire api METHOD` calls any daemon method; `periwire mcp` runs the MCP server that lets an AI assistant do the same with compact replies and human confirmation for risky actions."],
  ],
  cli: [
    ["periwire status", "Overview of everything"],
    ["periwire devices", "Local USB devices"],
    ["periwire share 2-2 --read-only", "Share a device (optionally write-protected)"],
    ["periwire remote HOST", "What another computer shares"],
    ["periwire attach HOST 2-2 --auto --read-only", "Use a remote device"],
    ["periwire detach 2-2", "Safely eject it"],
    ["periwire trust host HOST", "Pair: show a host's fingerprint, then trust it"],
    ["periwire id", "This computer's fingerprint"],
    ["periwire doctor", "Health checks"],
    ["periwire logs -f", "Follow the log"],
    ["periwire driver install", "Windows: install the USB driver"],
    ["periwire api status", "Call the daemon's JSON API"],
    ["periwire mcp", "Run the MCP server for AI assistants"],
  ],
};
