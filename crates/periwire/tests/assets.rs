//! The vendored usbip-win2 files must match their manifest, and every manifest entry must be embedded.
use sha2::{Digest, Sha256};
use std::path::Path;

fn manifest() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/usbip-win2");
    std::fs::read_to_string(dir.join("SHA256SUMS.txt")).unwrap().lines().filter_map(|l| l.split_once("  ").map(|(h, n)| (n.trim().to_string(), h.trim().to_string()))).collect()
}

#[test]
fn files_match_the_manifest() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/usbip-win2");
    let m = manifest();
    assert!(m.len() >= 10, "manifest looks empty");
    for (name, want) in m {
        let got = format!("{:x}", Sha256::digest(std::fs::read(dir.join(&name)).unwrap()));
        assert_eq!(got, want, "{name} was modified: the Microsoft signature would no longer verify");
    }
}

#[test]
fn every_manifest_file_is_embedded() {
    let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/windrv.rs")).unwrap();
    for (name, _) in manifest() {
        assert!(src.contains(&format!("asset!(\"{name}\")")), "{name} is vendored but not embedded in windrv.rs");
    }
    let version = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/usbip-win2/VERSION")).unwrap();
    assert!(src.contains(&format!("VERSION: &str = \"{}\"", version.trim())), "VERSION constant differs from assets/usbip-win2/VERSION");
}
