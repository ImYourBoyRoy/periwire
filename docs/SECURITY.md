# Security model

periwire gives another computer full control of a USB device — a flash drive, a security key, a keyboard. Treat trust
accordingly.


## Reporting a vulnerability

Please do not open a public issue for a security problem. Use GitHub's private reporting: on the repository open **Security > Report a vulnerability**
(https://github.com/ImYourBoyRoy/periwire/security/advisories/new). Include what you did, what you expected and what happened, and the PeriWire version
(`periwire --version`). You will get an answer as soon as I can give one; fixes for confirmed problems are released with credit unless you prefer otherwise.

## Authentication and encryption

`periwire identity reset` replaces the certificate if you think it leaked; peers must then re-trust the new fingerprint.

- Every computer has a self-signed certificate created on first run (`cert.der`/`key.der` in the periwire directory, key
  mode 0600). Its SHA-256 **fingerprint** is its identity.
- Connections are **mutual TLS 1.3**. Each side accepts a peer only if the peer's certificate fingerprint is in its
  trust list (`trusted.txt`). There is no certificate authority, no password and no account.
- Sharing and using are separate decisions: *A trusts B* lets B use A's shared devices; *B trusts A* lets B connect to A.
  Pair in both directions for the usual setup.
- Pairing is only as strong as the fingerprint comparison: `trust host` shows the fingerprint a host presents; you must
  compare it with `periwire id` on that computer (read it out, or compare on screen). Never skip this on a network you do
  not control. The MCP server will not trust a computer without an explicit human confirmation.

## What is exposed

- The exporter listens on TCP 3241 (configurable). An unauthenticated connection is dropped at the TLS handshake and
  cannot learn anything beyond "a periwire is here".
- Only devices you share are offered; others are invisible. A trusted peer can use a shared device exclusively (one peer
  at a time) and can send arbitrary USB requests to it — that is the point.
- Local control is a Unix socket protected by file permissions (see [API](API.md)). Whoever can open it controls the
  daemon. The daemon runs as root on Linux (needed to attach devices); keep the socket group small.
- The desktop app and MCP server make no network connections of their own.

## Connections that have not authenticated

Anyone who can reach the exporter's port can open a TCP connection without credentials, so the exporter limits what such a connection can cost: a TLS
handshake must finish within **10 seconds**, and at most **32** connections may be in the middle of a handshake at once; further ones are dropped immediately
(logged as "dropped"). A peer that does not present a pinned certificate never gets past the handshake (client certificates are mandatory). Verified live:
40 silent connections at once left 8 refused immediately and the other 32 closed at the time limit. A failure to read this computer's identity or trust list
while accepting costs that one connection, not the exporter.

## Dependencies

`cargo audit` reports **no known vulnerabilities**. It reports five advisories of the "unmaintained" or "unsound" kind (`glib` 0.18, `proc-macro-error`,
`zbus_names`, `zvariant`, `zvariant_derive`); all of them are in the Linux desktop app's GTK 3 and D-Bus tray stack that Tauri pulls in, none in the daemon, CLI or
protocol crates. The one soundness advisory (`glib::VariantStrIter`) concerns an iterator PeriWire never uses. They are tracked upstream and go away when Tauri
moves its Linux tray to GTK 4 / `glib-appindicator`. CI runs `cargo audit` on every change and on a schedule.

## Data safety

- periwire never reads, writes, formats or partitions a device on its own. It refuses to share a device that has a
  mounted filesystem on the sharing computer.
- *Eject* unmounts first and aborts if files are open. *Force*, *Disconnect* and *Unshare-while-in-use* are abrupt and
  can lose unsaved data on the device — the UI says so before doing them.

## Reconnect grace period

After an abruptly lost connection, the exporter keeps the drive claimed for up to `PERIWIRE_RECONNECT_GRACE` seconds (default 60) instead of
returning it to its own desktop, so the importer's automatic reconnect can resume before anything mounts the drive. Only importers that
asked for it (`--auto`) get this, and a deliberate detach still returns the drive at once. The held drive is resumed only for the same
certificate fingerprint and only with the same read-only and NTFS options, so a write-protected session can never come back writable or
the other way round; any other computer is refused until the hold ends. Nothing is written while a drive is held, and the control API reports
it as `reconnect_hold`. Stopping the daemon or disconnecting the drive from the sharing computer releases it immediately.

## Out of scope / known limits

- No revocation list beyond removing a fingerprint (`trust remove`); existing connections end at their next reconnect or
  when you `kick` them.
- Trust is per computer, not per device or per user; a trusted computer can use every shared device.
- No traffic padding or anonymity; peers see device descriptors.
- Compromised or malicious USB devices attached to a computer are as dangerous as if plugged in locally; only attach
  devices you would plug in.

Report vulnerabilities privately to the maintainer before disclosing publicly.

## Read-only imports

`attach --read-only` (and the GUI switch) asks the exporting computer to write-protect a USB storage device for that session.
The exporter sets the write-protect bit in the drive's reported mode pages and answers every command that is not on a short
list of known read-only SCSI commands locally, so a write never reaches the drive, whatever the importing OS or a raw-device
tool tries. It only restricts the importer that asked for it. Non-storage devices cannot be protected this way and are
refused in read-only mode. It protects the *data on the drive*; it is not a substitute for not trusting a peer, who could still
connect without asking for it unless you stop sharing the device.

**NTFS and Windows.** Windows will not mount an NTFS volume on a write-protected disk if the volume's journal was emptied (Linux's
`ntfs3` driver leaves it that way after any read-write mount): it must write to start the journal. So a Windows importer asks
the exporter for one more thing in read-only sessions (`FLAG_NTFS_JOURNAL`): if, and only if, the journal's first two pages are
found to be completely emptied, the exporter shows Windows the two restart pages Windows itself would write, in what it reads.
The drive is still never written and nothing is writable, so there is no scratch area in which files could be "saved" and then
lost. The pages are built from the journal's size alone; everything is bounds-checked, and anything the exporter does not
recognise (another sector size, an unusual layout, a failed read) means no change. A journal with *unfinished* changes cannot be
faked and is reported in the exporter's log instead. Only the Windows importer asks for this; `PERIWIRE_NTFS_JOURNAL=0` turns it
off and `=1` turns it on elsewhere.
