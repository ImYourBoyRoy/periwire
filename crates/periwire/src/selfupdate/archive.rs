//! Pulling the programs out of a release archive. Entries are chosen by file name only, so a path inside the archive can never decide where something is written.
use anyhow::{anyhow, bail, Result};
use std::io::Read;

/// The bytes of each wanted file (`name`), in the archive `bytes` (`.tar.gz`, or `.zip` on Windows).
pub fn extract(bytes: &[u8], asset: &str, wanted: &[String]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut found: Vec<(String, Vec<u8>)> = vec![];
    if asset.ends_with(".tar.gz") {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in tar.entries()? {
            let mut e = entry?;
            if !e.header().entry_type().is_file() {
                continue;
            }
            let name = e.path()?.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if wanted.contains(&name) && !found.iter().any(|(n, _)| *n == name) {
                let mut data = Vec::new();
                e.read_to_end(&mut data)?;
                found.push((name, data));
            }
        }
    } else if asset.ends_with(".zip") {
        #[cfg(windows)]
        {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            for i in 0..zip.len() {
                let mut f = zip.by_index(i)?;
                if f.is_dir() {
                    continue;
                }
                let name = f.name()?.rsplit('/').next().unwrap_or("").to_string();
                if wanted.contains(&name) && !found.iter().any(|(n, _)| *n == name) {
                    let mut data = Vec::new();
                    f.read_to_end(&mut data)?;
                    found.push((name, data));
                }
            }
        }
        #[cfg(not(windows))]
        bail!("zip archives are only unpacked on Windows");
    } else {
        bail!("do not know how to unpack {asset}");
    }
    for w in wanted {
        if !found.iter().any(|(n, _)| n == w) {
            return Err(anyhow!("the archive does not contain {w}"));
        }
    }
    Ok(found)
}
