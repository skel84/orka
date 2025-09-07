//! Orka kubehub (Milestone 0) – discovery and watcher wiring

#![forbid(unsafe_code)]

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use futures::TryStreamExt;
use kube::{
    api::Api,
    core::{DynamicObject, GroupVersionKind},
    discovery::{Discovery, Scope},
    runtime::watcher::{self, Event},
    Client,
};
use orka_core::{Delta, DeltaKind};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredResource {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub namespaced: bool,
}

impl DiscoveredResource {
    pub fn gvk_key(&self) -> String {
        if self.group.is_empty() {
            format!("{}/{}", self.version, self.kind)
        } else {
            format!("{}/{}/{}", self.group, self.version, self.kind)
        }
    }
}

/// Discover served resources (incl. CRDs) using kube Discovery.
pub async fn discover(_prefer_crd: bool) -> Result<Vec<DiscoveredResource>> {
    let client = Client::try_default().await?;
    let discovery = Discovery::new(client).run().await?;
    let mut out = Vec::new();
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            let namespaced = matches!(caps.scope, Scope::Namespaced);
            out.push(DiscoveredResource {
                group: ar.group.clone(),
                version: ar.version.clone(),
                kind: ar.kind.clone(),
                namespaced,
            });
        }
    }
    // Stable-ish order
    out.sort_by(|a, b| a.group.cmp(&b.group).then(a.version.cmp(&b.version)).then(a.kind.cmp(&b.kind)));
    Ok(out)
}

fn parse_gvk_key(key: &str) -> Result<GroupVersionKind> {
    let parts: Vec<_> = key.split('/').collect();
    match parts.as_slice() {
        [version, kind] => Ok(GroupVersionKind { group: String::new(), version: version.to_string(), kind: kind.to_string() }),
        [group, version, kind] => Ok(GroupVersionKind { group: (*group).to_string(), version: (*version).to_string(), kind: (*kind).to_string() }),
        _ => Err(anyhow!("invalid gvk key: {} (expect v1/Kind or group/v1/Kind)", key)),
    }
}

async fn find_api_resource(client: Client, gvk: &GroupVersionKind) -> Result<(kube::core::ApiResource, bool)> {
    let discovery = Discovery::new(client).run().await?;
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            if ar.group == gvk.group && ar.version == gvk.version && ar.kind == gvk.kind {
                let namespaced = matches!(caps.scope, Scope::Namespaced);
                return Ok((ar.clone(), namespaced));
            }
        }
    }
    Err(anyhow!("GVK not found: {}/{}/{}", gvk.group, gvk.version, gvk.kind))
}

fn strip_managed_fields(v: &mut serde_json::Value) {
    if let Some(meta) = v.get_mut("metadata") {
        if let Some(obj) = meta.as_object_mut() {
            obj.remove("managedFields");
        }
    }
}

fn to_uid(uid_str: &str) -> Result<orka_core::Uid> {
    let u = Uuid::parse_str(uid_str).context("parsing metadata.uid as uuid")?;
    let bytes = *u.as_bytes();
    Ok(bytes)
}

fn delta_from(obj: &DynamicObject, kind: DeltaKind) -> Result<Delta> {
    let uid_str = obj
        .metadata
        .uid
        .as_deref()
        .ok_or_else(|| anyhow!("object missing metadata.uid"))?;
    let uid = to_uid(uid_str)?;
    let mut raw = serde_json::to_value(obj).context("serializing DynamicObject")?;
    strip_managed_fields(&mut raw);
    Ok(Delta { uid, kind, raw })
}

/// Start list+watch for a given GVK key and send coalesced deltas into provided channel.
pub async fn start_watcher(gvk_key: &str, namespace: Option<&str>, delta_tx: mpsc::Sender<Delta>) -> Result<()> {
    let client = Client::try_default().await?;
    let gvk = parse_gvk_key(gvk_key)?;
    let (ar, namespaced) = find_api_resource(client.clone(), &gvk).await?;

    let api: Api<DynamicObject> = if namespaced {
        match namespace {
            Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
            None => Api::all_with(client.clone(), &ar),
        }
    } else {
        Api::all_with(client.clone(), &ar)
    };

    let cfg = watcher::Config::default();
    let stream = watcher::watcher(api, cfg);
    futures::pin_mut!(stream);
    info!(gvk = %gvk_key, ns = ?namespace, "watcher started");
    while let Some(ev) = stream.try_next().await? {
        match ev {
            Event::Applied(o) => {
                let d = delta_from(&o, DeltaKind::Applied)?;
                let _ = delta_tx.send(d).await;
            }
            Event::Deleted(o) => {
                let d = delta_from(&o, DeltaKind::Deleted)?;
                let _ = delta_tx.send(d).await;
            }
            Event::Restarted(list) => {
                debug!(count = list.len(), "watch restart");
                for o in list.iter() {
                    let d = delta_from(o, DeltaKind::Applied)?;
                    let _ = delta_tx.send(d).await;
                }
            }
        }
    }
    warn!("watcher stream ended");
    Ok(())
}
