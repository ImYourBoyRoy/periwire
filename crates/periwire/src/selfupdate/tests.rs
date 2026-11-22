use super::*;

#[test]
fn the_download_name_follows_the_release_contract() {
    let n = |os, arch, k| asset_name("0.2.0", os, arch, k).unwrap();
    assert_eq!(n("linux", "x86_64", Kind::Archive), "periwire-0.2.0-linux-x86_64.tar.gz");
    assert_eq!(n("linux", "x86_64", Kind::AppImage), "periwire_0.2.0_amd64.AppImage");
    assert_eq!(n("macos", "aarch64", Kind::Archive), "periwire-0.2.0-macos-arm64.tar.gz");
    assert_eq!(n("macos", "x86_64", Kind::Archive), "periwire-0.2.0-macos-x86_64.tar.gz");
    assert_eq!(n("windows", "x86_64", Kind::Archive), "periwire-0.2.0-windows-x86_64.zip");
    assert!(asset_name("0.2.0", "linux", "aarch64", Kind::Archive).is_err() && asset_name("0.2.0", "freebsd", "x86_64", Kind::Archive).is_err());
}

#[test]
fn the_expected_hash_comes_from_a_sidecar_or_the_combined_list() {
    let h = "a".repeat(64);
    let other = "b".repeat(64);
    assert_eq!(expected_hash(&format!("{h}  periwire-0.2.0-linux-x86_64.tar.gz\n"), "periwire-0.2.0-linux-x86_64.tar.gz"), Some(h.clone()), "a .sha256 sidecar");
    assert_eq!(expected_hash(&format!("{}\n", h.to_uppercase()), "x"), Some(h.clone()), "a bare hash");
    let list = format!("{other}  other.zip\n{h} *the.tar.gz\n");
    assert_eq!(expected_hash(&list, "the.tar.gz"), Some(h.clone()), "SHA256SUMS.txt, binary-mode marker and all");
    assert_eq!(expected_hash(&list, "missing.zip"), None);
    assert_eq!(expected_hash("not a hash at all", "x"), None);
    assert_eq!(expected_hash(&format!("{}\n", "z".repeat(64)), "x"), None);
}

#[test]
fn what_can_be_updated_where() {
    assert!(matches!(target(Path::new("/usr/bin/periwire"), None), Target::Elsewhere(m) if m.contains("package")));
    assert!(matches!(target(Path::new("/Applications/PeriWire.app/Contents/MacOS/periwire"), None), Target::Elsewhere(m) if m.contains("disk image")));
    assert_eq!(target(Path::new("/tmp/x/periwire"), Some("/home/u/PeriWire.AppImage".into())), Target::AppImage("/home/u/PeriWire.AppImage".into()));
    let d = std::env::temp_dir().join(format!("pw-su-target-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let (cli, gui) = exe_names();
    std::fs::write(d.join(cli), b"x").unwrap();
    std::fs::write(d.join(gui), b"y").unwrap();
    let Target::Folder { files } = target(&d.join(cli), None) else { panic!("a folder install") };
    assert_eq!(files.len(), 2, "the program and the app beside it");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn base_urls_are_https_file_or_loopback_http() {
    for ok in ["https://example.org/r/", "file:///tmp/mirror", "http://127.0.0.1:8000/x", "http://localhost/x", "http://[::1]:9/x"] {
        assert!(fetch::validate_base(ok).is_ok(), "{ok}");
    }
    for bad in ["http://example.org/x", "ftp://x", "http://127.0.0.1.evil.org/x", "", "/tmp/x"] {
        assert!(fetch::validate_base(bad).is_err(), "{bad}");
    }
    assert_eq!(fetch::file_path_for_test("file:///C:/mirror/a.zip"), "C:/mirror/a.zip");
    assert_eq!(fetch::file_path_for_test("file:///tmp/m/a"), "/tmp/m/a");
}

fn tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (name, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        tar.append_data(&mut h, format!("periwire-9.9.9-linux-x86_64/{name}"), *data).unwrap();
    }
    let raw = tar.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &raw).unwrap();
    gz.finish().unwrap()
}

#[test]
fn only_the_wanted_files_are_taken_from_an_archive_and_missing_ones_are_an_error() {
    let t = tarball(&[("periwire", b"new cli"), ("periwire-gui", b"new gui"), ("README.md", b"docs")]);
    let got = archive::extract(&t, "a.tar.gz", &["periwire".into(), "periwire-gui".into()]).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got.iter().find(|(n, _)| n == "periwire").unwrap().1, b"new cli");
    assert!(archive::extract(&t, "a.tar.gz", &["periwire-tray".into()]).is_err());
    assert!(archive::extract(b"junk", "a.tar.gz", &["periwire".into()]).is_err());
    assert!(archive::extract(&t, "a.7z", &["periwire".into()]).is_err());
}

#[test]
#[cfg(unix)]
fn a_folder_install_is_replaced_from_a_mirror_and_a_bad_checksum_changes_nothing() {
    let d = std::env::temp_dir().join(format!("pw-su-run-{}", std::process::id()));
    let (app, mirror) = (d.join("app"), d.join("mirror"));
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir_all(&mirror).unwrap();
    std::fs::write(app.join("periwire"), b"old cli").unwrap();
    std::fs::write(app.join("periwire-gui"), b"old gui").unwrap();
    let asset = asset_name("9.9.9", std::env::consts::OS, std::env::consts::ARCH, Kind::Archive);
    let Ok(asset) = asset else { return };
    if !asset.ends_with(".tar.gz") {
        return;
    }
    let t = tarball(&[("periwire", b"new cli"), ("periwire-gui", b"new gui")]);
    std::fs::write(mirror.join(&asset), &t).unwrap();
    let sums = mirror.join(format!("{asset}.sha256"));
    std::fs::write(&sums, format!("{}  {asset}\n", "0".repeat(64))).unwrap();
    let files = vec![app.join("periwire"), app.join("periwire-gui")];
    let base = format!("file://{}", mirror.display());
    let run = || install_from(&base, &asset, &Target::Folder { files: files.clone() });
    assert!(run().unwrap_err().to_string().contains("does not match"), "a wrong checksum is refused");
    assert_eq!(std::fs::read(app.join("periwire")).unwrap(), b"old cli", "and nothing was touched");
    std::fs::write(&sums, format!("{}  {asset}\n", sha256_hex(&t))).unwrap();
    run().unwrap();
    assert_eq!(std::fs::read(app.join("periwire")).unwrap(), b"new cli");
    assert_eq!(std::fs::read(app.join("periwire-gui")).unwrap(), b"new gui");
    assert!(!app.join("periwire.new").exists(), "no staging files are left behind");
    std::fs::remove_file(&sums).unwrap();
    assert!(run().unwrap_err().to_string().contains("no checksum"), "no checksum, no install");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_support_report_says_how_each_kind_updates() {
    let s = support();
    assert!(s["supported"].is_boolean() && s["kind"].is_string(), "{s}");
}
