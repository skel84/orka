#![forbid(unsafe_code)]

use orka_api::ResourceKind;

pub mod highlight;

pub(crate) fn gvk_label(k: &ResourceKind) -> String {
    if k.group.is_empty() { format!("{}/{}", k.version, k.kind) } else { format!("{}/{}/{}", k.group, k.version, k.kind) }
}

pub(crate) fn parse_gvk_key_to_kind(key: &str) -> ResourceKind {
    let parts: Vec<&str> = key.split('/').collect();
    match parts.as_slice() {
        [version, kind] => ResourceKind { group: String::new(), version: (*version).to_string(), kind: (*kind).to_string(), namespaced: true },
        [group, version, kind] => ResourceKind { group: (*group).to_string(), version: (*version).to_string(), kind: (*kind).to_string(), namespaced: true },
        _ => ResourceKind { group: String::new(), version: String::new(), kind: key.to_string(), namespaced: true },
    }
}

pub(crate) fn render_age(creation_ts: i64) -> String {
    if creation_ts <= 0 {
        return "-".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mut secs = (now - creation_ts).max(0) as u64;
    let days = secs / 86_400;
    secs %= 86_400;
    let hours = secs / 3600;
    secs %= 3600;
    let mins = secs / 60;
    secs %= 60;
    if days > 0 {
        format!("{}d{}h", days, hours)
    } else if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else if mins > 0 {
        format!("{}m", mins)
    } else {
        format!("{}s", secs)
    }
}
