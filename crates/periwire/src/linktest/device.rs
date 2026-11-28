//! Linking the check to a device: what it needs from the link (from the exporter's device list), and the check made before attaching one.
use super::*;

/// The link requirements a device card carries (`iso` in the device views), if the device has isochronous endpoints.
pub fn need_from_view(device: &Value) -> Option<Need> {
    let iso = device.get("iso").filter(|v| v.is_object())?;
    let kbps = |k: &str| iso.get(k).and_then(Value::as_u64).unwrap_or(0).min(u64::from(u32::MAX)) as u32;
    let kind = match iso.get("kind").and_then(Value::as_str) {
        Some("audio") => Kind::Audio,
        Some("video") => Kind::Video,
        _ => Kind::Other,
    };
    Some(Need { kind, in_kbps: kbps("in_kbps"), out_kbps: kbps("out_kbps") })
}

/// The device `busid` as `addr` describes it, if that computer shares it.
pub async fn find_device(addr: &str, busid: &str) -> Result<Option<Value>> {
    Ok(crate::import::list_remote(addr).await?.into_iter().find(|d| d["busid"] == busid))
}

/// Check the link for `busid` on `addr` and return the report as JSON with the device's own numbers added. A device without
/// isochronous endpoints still gets a check, just without a particular need to judge it against.
pub async fn check_device(addr: &str, busid: &str, full_check: bool) -> Result<Value> {
    let device = find_device(addr, busid).await?;
    let need = device.as_ref().and_then(need_from_view);
    let req = if full_check { full(need.as_ref()) } else { quick(need.as_ref()) };
    let mut v = run(addr, req, need.as_ref()).await?.to_json();
    if let (Some(o), Some(n)) = (v.as_object_mut(), &need) {
        o.insert("need".into(), serde_json::to_value(n)?);
        o.insert("busid".into(), busid.into());
    }
    Ok(v)
}

/// Before attaching: if the device uses isochronous transfers (camera, audio), a quick check whose answer goes with the attach result.
/// Never fails the attach and never takes more than a few seconds; `None` means nothing to say.
pub async fn before_attach(addr: &str, busid: &str) -> Option<Value> {
    if matches!(std::env::var("PERIWIRE_LINKCHECK").as_deref(), Ok("0") | Ok("off")) {
        return None;
    }
    let device = tokio::time::timeout(Duration::from_secs(4), find_device(addr, busid)).await.ok()?.ok()??;
    let need = need_from_view(&device)?;
    let report = tokio::time::timeout(Duration::from_secs(6), run(addr, quick(Some(&need)), Some(&need))).await.ok()?.ok()?;
    let mut v = report.to_json();
    v.as_object_mut()?.insert("need".into(), serde_json::to_value(&need).ok()?);
    Some(v)
}

/// The `link.test` API method: a check of the link to `addr`, for a device if `busid` is given.
pub async fn api_test(addr: &str, busid: Option<&str>, full_check: bool) -> Result<Value> {
    match busid.filter(|b| !b.is_empty()) {
        Some(busid) => check_device(addr, busid, full_check).await,
        None => Ok(run(addr, if full_check { full(None) } else { quick(None) }, None).await?.to_json()),
    }
}

/// Put the pre-attach check into an attach result as `link`, and its notes as `warnings` when the link is weak for the device.
pub fn add_to_attach(view: &mut Value, busid: &str, addr: &str, link: Option<Value>) {
    let (Some(o), Some(l)) = (view.as_object_mut(), link) else { return };
    if l["verdict"]["level"] != "good" {
        crate::logln!("{busid} from {addr}: {}", l["verdict"]["headline"].as_str().unwrap_or("the connection is weak for this device"));
        o.insert("warnings".into(), l["verdict"]["notes"].clone());
    }
    o.insert("link".into(), l);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_weak_link_adds_warnings_to_the_attach_result_and_a_good_one_only_the_report() {
        let weak = json!({"verdict": {"level": "poor", "headline": "too weak", "notes": ["a", "b"]}});
        let mut v = json!({"busid": "1-1"});
        add_to_attach(&mut v, "1-1", "h", Some(weak));
        assert_eq!(v["warnings"], json!(["a", "b"]));
        assert_eq!(v["link"]["verdict"]["level"], "poor");
        let mut v = json!({"busid": "1-1"});
        add_to_attach(&mut v, "1-1", "h", Some(json!({"verdict": {"level": "good", "notes": []}})));
        assert!(v.get("warnings").is_none() && v["link"].is_object());
        let mut v = json!({"busid": "1-1"});
        add_to_attach(&mut v, "1-1", "h", None);
        assert!(v.get("link").is_none(), "no check, nothing added");
    }

    #[test]
    fn a_device_card_becomes_a_need_only_when_it_has_isochronous_endpoints() {
        assert_eq!(need_from_view(&json!({"iso": null})), None);
        assert_eq!(need_from_view(&json!({"busid": "1-1"})), None);
        let n = need_from_view(&json!({"iso": {"kind": "video", "in_kbps": 48000, "out_kbps": 0}})).unwrap();
        assert_eq!((n.kind, n.in_kbps, n.out_kbps), (Kind::Video, 48_000, 0));
        assert_eq!(need_from_view(&json!({"iso": {"kind": "weird", "in_kbps": 5}})).unwrap().kind, Kind::Other);
    }
}
