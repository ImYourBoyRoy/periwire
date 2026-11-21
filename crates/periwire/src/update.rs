//! Is a newer PeriWire out? One HTTPS request to GitHub's "latest release" endpoint, nothing sent but a User-Agent.
//! This only *tells* the user and links to the release page; it installs nothing.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

const API: &str = "https://api.github.com/repos/ImYourBoyRoy/periwire/releases/latest";
/// The only place a reported release link may point; the GUI opens it in the browser.
const RELEASES_PREFIX: &str = "https://github.com/ImYourBoyRoy/periwire/releases/";

/// `v1.2.3`, `1.2.3` or `1.2.3-rc.1` -> (1, 2, 3, is_release). A pre-release sorts before the release it leads up to.
fn parse(v: &str) -> Option<(u64, u64, u64, bool)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let (core, release) = match v.split_once(['-', '+']) {
        Some((c, _)) => (c, false),
        None => (v, true),
    };
    let mut it = core.split('.');
    let (a, b, c) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    it.next().is_none().then_some((a, b, c, release))
}

/// Is `latest` a higher version than `current`? Anything unparsable is "no".
pub fn is_newer(current: &str, latest: &str) -> bool {
    matches!((parse(current), parse(latest)), (Some(c), Some(l)) if l > c)
}

/// What the check found.
pub fn info_from_release(release: &Value, current: &str) -> Result<Value> {
    let tag = release["tag_name"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| anyhow!("the release has no tag"))?;
    let url = release["html_url"].as_str().unwrap_or("");
    if !url.starts_with(RELEASES_PREFIX) {
        bail!("the release page is not on github.com/ImYourBoyRoy/periwire: {url}");
    }
    let latest = tag.trim_start_matches(['v', 'V']);
    Ok(json!({
        "current": current,
        "latest": latest,
        "newer": is_newer(current, latest),
        "url": url,
        "published": release["published_at"].as_str(),
    }))
}

/// Ask GitHub (blocking, at most a few seconds). Run it on a blocking thread from async code.
pub fn check_blocking(current: &str) -> Result<Value> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(8)).build();
    let response = agent.get(API).set("Accept", "application/vnd.github+json").set("User-Agent", &format!("periwire/{current}")).call().map_err(|e| match e {
        ureq::Error::Status(404, _) => anyhow!("no release has been published yet"),
        ureq::Error::Status(403 | 429, _) => anyhow!("GitHub is limiting requests from this network; try again later"),
        other => anyhow!("could not reach GitHub: {other}"),
    })?;
    let body = response.into_string().context("could not read GitHub's answer")?;
    let release: Value = serde_json::from_str(&body).context("GitHub's answer was not JSON")?;
    info_from_release(&release, current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_the_way_people_expect() {
        assert!(is_newer("0.1.0", "0.1.1") && is_newer("0.1.9", "0.2.0") && is_newer("0.9.0", "v1.0.0"));
        assert!(!is_newer("0.1.10", "0.1.9") && is_newer("0.1.2", "0.1.10"), "numbers, not text");
        assert!(!is_newer("0.1.1", "0.1.1") && !is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.2.0-rc.1", "0.2.0"), "the release beats its own release candidate");
        assert!(!is_newer("0.2.0", "0.2.0-rc.1"));
        assert!(!is_newer("0.1.0", "banana") && !is_newer("", "0.1.1") && !is_newer("0.1", "0.2.0"));
    }

    #[test]
    fn a_release_is_summarised_and_only_known_pages_are_linked() {
        let rel = json!({"tag_name": "v0.2.0", "html_url": "https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.2.0", "published_at": "2026-12-01T00:00:00Z"});
        let i = info_from_release(&rel, "0.1.1").unwrap();
        assert_eq!((i["latest"].as_str(), i["newer"].as_bool()), (Some("0.2.0"), Some(true)));
        assert_eq!(info_from_release(&rel, "0.2.0").unwrap()["newer"], json!(false));
        let evil = json!({"tag_name": "v9.9.9", "html_url": "https://evil.example/releases/tag/v9.9.9"});
        assert!(info_from_release(&evil, "0.1.1").is_err(), "a link anywhere else is refused");
        assert!(info_from_release(&json!({"html_url": "https://github.com/ImYourBoyRoy/periwire/releases/"}), "0.1.1").is_err(), "no tag, no answer");
    }
}
