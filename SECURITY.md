# Security policy

PeriWire shares USB devices between computers, so a security bug can expose someone's files or hardware. Please report
problems privately.

## Reporting a vulnerability

Use GitHub's private reporting: open the **Security** tab of
[ImYourBoyRoy/periwire](https://github.com/ImYourBoyRoy/periwire/security/advisories/new) and choose *Report a vulnerability*.
Please do not open a public issue for it. Include the version (`periwire --version`), your operating systems, and the
smallest set of steps that shows the problem.

You can expect a first reply within a week. Fixes ship in a new release, and the advisory credits you unless you prefer
not to be named.

## Supported versions

Only the latest release is supported while the project is at 0.x.

## What is in scope

Anything that lets a computer or person who is **not** trusted use, see or disturb a shared device, bypass pairing, read
or alter the identity and trust files, or run code through the daemon, the control socket, the MCP server or the desktop
app. The [security model](docs/SECURITY.md) lists what PeriWire protects and what it deliberately trusts.

## Verifying a download

Every release file is covered by `SHA256SUMS.txt`, a CycloneDX dependency list (`periwire.cdx.json`) and a signed build
attestation that names the workflow run that built it:

```bash
sha256sum -c SHA256SUMS.txt --ignore-missing
gh attestation verify <downloaded-file> --repo ImYourBoyRoy/periwire
```

The macOS and Windows programs are not yet signed with a paid developer certificate; the attestation is how you check
where they came from.
