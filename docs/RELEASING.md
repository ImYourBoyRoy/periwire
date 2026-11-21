# Releasing

How a release is made, and the file names other things depend on.

## Steps

1. Update `CHANGELOG.md`; set the version in the root `Cargo.toml` (`[workspace.package]`) and in `gui/src-tauri/tauri.conf.json`; run `cargo build` so `Cargo.lock` follows.
2. Make sure CI is green on that commit, then tag it: `git tag vX.Y.Z <commit> && git push origin vX.Y.Z`.
3. The **Release** workflow builds every platform and drafts the GitHub release with the files below, a CycloneDX dependency list (`periwire.cdx.json`),
   a checksum file next to every download and signed build-provenance attestations. Check the draft's files, edit the notes, then publish it.
4. Verify: `sha256sum -c SHA256SUMS.txt --ignore-missing`, `gh attestation verify <file> --repo ImYourBoyRoy/periwire`, and run a downloaded binary (`periwire --version`).

## File names are a contract

imyourboyroy.com always links to the **latest** release: it asks GitHub's `releases/latest`, picks each download by a name pattern
(`downloadRules` in that site's `src/data/software-projects.json`, entry `periwire`) and links the checksum file `<download>.sha256` beside it.
Keep these name endings, one file each, in every release (the Release workflow fails if one is missing or ambiguous):

| Website button | Name ends with | Example |
|---|---|---|
| Windows | `x64-setup.exe` | `PeriWire_0.1.1_x64-setup.exe` |
| macOS Apple silicon | `aarch64.dmg` | `PeriWire_0.1.1_aarch64.dmg` |
| macOS Intel | `_x64.dmg` | `PeriWire_0.1.1_x64.dmg` |
| Linux AppImage | `.AppImage` | `periwire_0.1.1_amd64.AppImage` |
| Linux Debian | `_amd64.deb` | `periwire_0.1.1_amd64.deb` |

The version is in the name but the website never needs it: the tag (`vX.Y.Z`) is what it shows, and `releases/latest` ignores drafts and pre-releases.
The portable archives (`periwire-X.Y.Z-<os>-<arch>.tar.gz` / `.zip`) are not on the website; they are for scripts and the command line.
If a name must change, update the rule on the website in the same change.

## Update check

`periwire update`, the desktop app and the MCP tool `periwire_update_check` read the same `releases/latest` endpoint and compare versions
(`vX.Y.Z`, numbers not text, a pre-release sorts before its release). They only report and link; installing stays manual until a signed self-updater exists.
