//! Getting the release files: https from GitHub, or a folder you point `PERIWIRE_RELEASE_BASE_URL` at (https, http on loopback only, or file://).
use anyhow::{anyhow, bail, Context, Result};
use std::io::Read;
use std::time::Duration;

/// Check that a base URL is one this tool will talk to.
pub fn validate_base(base: &str) -> Result<String> {
    let b = base.trim().trim_end_matches('/').to_string();
    let loopback = |rest: &str| {
        let host = rest.split(['/', ':']).next().unwrap_or("");
        let host = if rest.starts_with('[') { rest.split(']').next().unwrap_or("").trim_start_matches('[') } else { host };
        matches!(host, "127.0.0.1" | "localhost" | "::1")
    };
    if b.starts_with("https://") || b.starts_with("file://") || b.strip_prefix("http://").is_some_and(loopback) {
        Ok(b)
    } else {
        bail!("PERIWIRE_RELEASE_BASE_URL must be https://, file://, or http:// on this computer (127.0.0.1/localhost); got {base}")
    }
}

/// The folder holding a release's files.
pub fn base_url(version: &str) -> Result<String> {
    match std::env::var("PERIWIRE_RELEASE_BASE_URL") {
        Ok(v) if !v.trim().is_empty() => validate_base(&v),
        _ => Ok(format!("https://github.com/ImYourBoyRoy/periwire/releases/download/v{version}")),
    }
}

/// `file:///C:/x/y` -> `C:/x/y`, `file:///home/x` -> `/home/x`.
fn file_path(url: &str) -> String {
    let p = url.strip_prefix("file://").unwrap_or(url);
    let p = p.strip_prefix('/').filter(|r| r.as_bytes().get(1) == Some(&b':')).unwrap_or(p);
    p.to_string()
}

/// Download (or read) `url`, refusing more than `max` bytes.
pub fn get(url: &str, max: u64) -> Result<Vec<u8>> {
    if url.starts_with("file://") {
        let p = file_path(url);
        let meta = std::fs::metadata(&p).with_context(|| format!("reading {p}"))?;
        if meta.len() > max {
            bail!("{p} is larger than {max} bytes");
        }
        return std::fs::read(&p).with_context(|| format!("reading {p}"));
    }
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(60)).build();
    let resp = agent.get(url).set("User-Agent", concat!("periwire/", env!("CARGO_PKG_VERSION"))).call().map_err(|e| match e {
        ureq::Error::Status(c, _) => anyhow!("{url}: the server answered {c}"),
        other => anyhow!("{url}: {other}"),
    })?;
    let mut out = Vec::new();
    resp.into_reader().take(max + 1).read_to_end(&mut out)?;
    if out.len() as u64 > max {
        bail!("{url} is larger than {max} bytes");
    }
    Ok(out)
}

#[cfg(test)]
pub(super) fn file_path_for_test(url: &str) -> String {
    file_path(url)
}
