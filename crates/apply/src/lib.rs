//! Orka apply (Milestone 2): dry-run and SSA helpers + minimal diffs.

#![forbid(unsafe_code)]

use anyhow::{anyhow, Context, Result};
use kube::{api::{Api, Patch, PatchParams}, core::{DynamicObject, GroupVersionKind}, discovery::{Discovery, Scope}, Client};
use metrics::{counter, histogram};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tracing::warn;
use orka_persist::Store;
use uuid::Uuid;

fn max_yaml_bytes() -> usize {
    std::env::var("ORKA_MAX_YAML_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1_000_000) // 1 MiB default
}

fn max_yaml_nodes() -> usize {
    std::env::var("ORKA_MAX_YAML_NODES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100_000)
}

fn json_node_budget_exceeded(v: &Json, max: usize) -> bool {
    // Fast precheck: keep a running counter and bail early when exceeding max
    fn walk(v: &Json, cur: &mut usize, max: usize) {
        if *cur >= max { return; }
        *cur += 1;
        match v {
            Json::Object(map) => {
                for (_k, vv) in map.iter() {
                    if *cur >= max { break; }
                    walk(vv, cur, max);
                }
            }
            Json::Array(arr) => {
                for vv in arr.iter() {
                    if *cur >= max { break; }
                    walk(vv, cur, max);
                }
            }
            _ => {}
        }
    }
    let mut count = 0usize;
    walk(v, &mut count, max);
    count >= max
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiffSummary { pub adds: usize, pub updates: usize, pub removes: usize }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyResult {
    pub dry_run: bool,
    pub applied: bool,
    pub new_rv: Option<String>,
    pub warnings: Vec<String>,
    pub summary: DiffSummary,
}

pub async fn edit_from_yaml(
    yaml: &str,
    ns_override: Option<&str>,
    validate: bool,
    do_apply: bool,
) -> Result<ApplyResult> {
    let t0 = std::time::Instant::now();
    counter!("apply_attempts", 1u64);
    let (json, gvk, name, ns) = parse_yaml_for_target(yaml, ns_override)?;

    if validate {
        // M2: validation optional and only for CRDs; keep soft-fail with messages routed to CLI if needed.
        // We do not wire jsonschema here to avoid heavy deps in this crate. CLI will call into orka_schema feature if enabled.
    }

    let client = orka_kubehub::get_kube_client().await?;
    let (ar, namespaced) = find_api_resource(client.clone(), &gvk).await?;
    let api: Api<DynamicObject> = if namespaced {
        match ns.as_deref() {
            Some(n) => Api::namespaced_with(client.clone(), n, &ar),
            None => return Err(anyhow!("namespace required for namespaced kind")),
        }
    } else {
        Api::all_with(client.clone(), &ar)
    };

    // Load live to compute diff summary
    let live_json = match api.get_opt(&name).await? {
        Some(obj) => Some(strip_noisy(serde_json::to_value(&obj)?)),
        None => None,
    };
    let tgt_json = {
        let mut v = strip_noisy(json.clone());
        ensure_metadata(&mut v, &name, ns.as_deref());
        v
    };
    let summary = diff_summary(&tgt_json, &live_json.clone().unwrap_or(Json::Null));

    if !do_apply {
        // Dry-run: ask server to validate the SSA patch but don't persist
        let pp = PatchParams::apply("orka").dry_run();
        let res = api.patch(&name, &pp, &Patch::Apply(&json)).await;
        match res {
            Ok(_) => {
                histogram!("apply_latency_ms", t0.elapsed().as_secs_f64() * 1000.0);
                counter!("apply_dry_ok", 1u64);
                return Ok(ApplyResult { dry_run: true, applied: false, new_rv: None, warnings: vec![], summary });
            }
            Err(e) => {
                counter!("apply_err", 1u64);
                return Err(anyhow!("dry-run failed: {}", e));
            }
        }
    }

    // Optional preflight freshness guard: if live resourceVersion changed since we computed the diff, abort.
    // Enabled by default; set ORKA_DISABLE_APPLY_PREFLIGHT=1 to skip this guard.
    if std::env::var("ORKA_DISABLE_APPLY_PREFLIGHT").is_err() {
        if let Some(prev_live) = &live_json {
            let prev_rv = prev_live
                .get("metadata").and_then(|m| m.get("resourceVersion")).and_then(|v| v.as_str()).map(|s| s.to_string());
            if let Some(prev_rv) = prev_rv {
                if let Some(obj2) = api.get_opt(&name).await? {
                    let cur_rv = obj2.metadata.resource_version.clone().unwrap_or_default();
                    if !cur_rv.is_empty() && cur_rv != prev_rv {
                        metrics::counter!("apply_stale_blocked_total", 1u64);
                        return Err(anyhow!(
                            "live object changed (rv {} -> {}) during apply; re-run diff/dry-run and try again",
                            prev_rv, cur_rv
                        ));
                    }
                }
            }
        }
    }

    // Real apply (SSA)
    let pp = PatchParams::apply("orka");
    let obj = match api.patch(&name, &pp, &Patch::Apply(&json)).await {
        Ok(o) => o,
        Err(e) => { counter!("apply_err", 1u64); return Err(anyhow!("server-side apply failed: {}", e)); }
    };
    let new_rv = obj.metadata.resource_version.clone();
    histogram!("apply_latency_ms", t0.elapsed().as_secs_f64() * 1000.0);
    counter!("apply_ok", 1u64);

    // Persist last-applied YAML snapshot (zstd if enabled), with safety guards
    let disable_persist = std::env::var("ORKA_DISABLE_LASTAPPLIED")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let is_secret = gvk.group.is_empty() && gvk.kind == "Secret";
    if !disable_persist && !is_secret {
        let uid = obj.metadata.uid.as_deref().ok_or_else(|| anyhow!("applied object missing metadata.uid"))?;
        let uid_bin = parse_uid(uid)?;
        let rv = new_rv.clone().unwrap_or_default();
        let la = orka_persist::LastApplied { uid: uid_bin, rv, ts: orka_persist::now_ts(), yaml_zstd: orka_persist::maybe_compress(yaml) };
        match orka_persist::LogStore::open_default() {
            Ok(store) => { let _ = store.put_last(la); }
            Err(e) => warn!(error = %e, "persist open failed; skipping last-applied save"),
        }
    } else if is_secret {
        warn!("skipping last-applied persist for Secret kind");
    }

    Ok(ApplyResult { dry_run: false, applied: true, new_rv, warnings: vec![], summary })
}

pub async fn diff_from_yaml(yaml: &str, ns_override: Option<&str>) -> Result<(DiffSummary, Option<DiffSummary>)> {
    let (json, gvk, name, ns) = parse_yaml_for_target(yaml, ns_override)?;
    let client = orka_kubehub::get_kube_client().await?;
    let (ar, namespaced) = find_api_resource(client.clone(), &gvk).await?;
    let api: Api<DynamicObject> = if namespaced {
        match ns.as_deref() {
            Some(n) => Api::namespaced_with(client.clone(), n, &ar),
            None => return Err(anyhow!("namespace required for namespaced kind")),
        }
    } else {
        Api::all_with(client.clone(), &ar)
    };
    let live_json = match api.get_opt(&name).await? {
        Some(obj) => Some(strip_noisy(serde_json::to_value(&obj)?)),
        None => None,
    };
    let tgt_json = {
        let mut v = strip_noisy(json.clone());
        ensure_metadata(&mut v, &name, ns.as_deref());
        v
    };
    let live_summary = diff_summary(&tgt_json, &live_json.clone().unwrap_or(Json::Null));

    // Diff against last-applied if present
    let last_summary = if let Some(uid_str) = live_json.as_ref().and_then(|v| v.get("metadata")).and_then(|m| m.get("uid")).and_then(|s| s.as_str()) {
        if let Ok(store) = orka_persist::LogStore::open_default() {
            if let Ok(rows) = store.get_last(parse_uid(uid_str)?, Some(1)) {
                if let Some(top) = rows.first() {
                    let prev_yaml = orka_persist::maybe_decompress(&top.yaml_zstd);
                    if let Ok(prev_val_yaml) = serde_yaml::from_str::<serde_yaml::Value>(&prev_yaml) {
                        let prev_json = serde_json::to_value(prev_val_yaml).unwrap_or(Json::Null);
                        let prev = strip_noisy(prev_json);
                        let sum = diff_summary(&tgt_json, &prev);
                        Some(sum)
                    } else { None }
                } else { None }
            } else { None }
        } else { None }
    } else { None };

    Ok((live_summary, last_summary))
}

fn parse_yaml_for_target(yaml: &str, ns_override: Option<&str>) -> Result<(Json, GroupVersionKind, String, Option<String>)> {
    if yaml.len() > max_yaml_bytes() {
        return Err(anyhow!("YAML payload too large (>{} bytes)", max_yaml_bytes()));
    }
    let val: serde_yaml::Value = serde_yaml::from_str(yaml).context("parsing YAML")?;
    let json = serde_json::to_value(val).context("converting YAML to JSON")?;
    if json_node_budget_exceeded(&json, max_yaml_nodes()) {
        return Err(anyhow!("YAML document too complex (>{} nodes)", max_yaml_nodes()));
    }
    let api_version_s = json.get("apiVersion").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("YAML missing apiVersion"))?.to_string();
    let kind_s = json.get("kind").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("YAML missing kind"))?.to_string();
    let (group, version) = if let Some((g, v)) = api_version_s.split_once('/') { (g.to_string(), v.to_string()) } else { (String::new(), api_version_s) };
    let name = json.get("metadata").and_then(|m| m.get("name")).and_then(|v| v.as_str()).ok_or_else(|| anyhow!("YAML missing metadata.name"))?.to_string();
    let ns = ns_override.map(|s| s.to_string()).or_else(|| json.get("metadata").and_then(|m| m.get("namespace")).and_then(|v| v.as_str()).map(|s| s.to_string()));
    Ok((json, GroupVersionKind { group, version, kind: kind_s }, name, ns))
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

fn strip_noisy(mut v: Json) -> Json {
    if let Some(meta) = v.get_mut("metadata") {
        if let Some(obj) = meta.as_object_mut() {
            obj.remove("managedFields");
            obj.remove("resourceVersion");
            obj.remove("generation");
            obj.remove("creationTimestamp");
        }
    }
    // Status is server-populated; ignore it during diffs
    if let Some(obj) = v.as_object_mut() { obj.remove("status"); }
    v
}

fn ensure_metadata(v: &mut Json, name: &str, ns: Option<&str>) {
    let meta = v.as_object_mut().unwrap().entry("metadata").or_insert(Json::Object(serde_json::Map::new()));
    if let Some(obj) = meta.as_object_mut() {
        obj.insert("name".into(), Json::String(name.to_string()));
        if let Some(ns) = ns { obj.insert("namespace".into(), Json::String(ns.to_string())); }
    }
}

pub fn diff_summary(target: &Json, base: &Json) -> DiffSummary {
    fn walk(a: &Json, b: &Json, adds: &mut usize, ups: &mut usize, rems: &mut usize) {
        use serde_json::Value as V;
        match (a, b) {
            (V::Object(ao), V::Object(bo)) => {
                for (k, av) in ao.iter() {
                    if let Some(bv) = bo.get(k) {
                        if av == bv { continue; }
                        walk(av, bv, adds, ups, rems);
                    } else {
                        *adds += 1;
                    }
                }
                for (k, _bv) in bo.iter() {
                    if !ao.contains_key(k) { *rems += 1; }
                }
            }
            (V::Array(aa), V::Array(bb)) => {
                let min_len = aa.len().min(bb.len());
                for i in 0..min_len { if aa[i] != bb[i] { *ups += 1; } }
                if aa.len() > bb.len() { *adds += aa.len() - bb.len(); }
                if bb.len() > aa.len() { *rems += bb.len() - aa.len(); }
            }
            // Scalars differ or type differs
            (av, bv) => { if av != bv { *ups += 1; } }
        }
    }
    let mut adds = 0usize; let mut ups = 0usize; let mut rems = 0usize;
    walk(target, base, &mut adds, &mut ups, &mut rems);
    DiffSummary { adds, updates: ups, removes: rems }
}

fn parse_uid(uid_str: &str) -> Result<[u8; 16]> {
    let u = Uuid::parse_str(uid_str).context("parsing metadata.uid as uuid")?;
    Ok(*u.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_noisy_prunes_common_fields() {
        let v = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": "x",
                "namespace": "ns",
                "managedFields": [ {"foo": "bar"} ],
                "resourceVersion": "123",
                "generation": 5,
                "creationTimestamp": "2020-01-01T00:00:00Z"
            },
            "status": { "obs": true },
            "data": { "k": "v" }
        });
        let pruned = strip_noisy(v);
        let meta = pruned.get("metadata").unwrap().as_object().unwrap();
        assert!(!meta.contains_key("managedFields"));
        assert!(!meta.contains_key("resourceVersion"));
        assert!(!meta.contains_key("generation"));
        assert!(!meta.contains_key("creationTimestamp"));
        assert!(!pruned.as_object().unwrap().contains_key("status"));
    }

    #[test]
    fn diff_summary_counts_adds_updates_removes() {
        let base = serde_json::json!({
            "a": 1,
            "b": { "x": 1 },
            "c": [1, 2, 3]
        });
        let target = serde_json::json!({
            "a": 2,                // scalar update
            "b": { "x": 1, "y": 2 }, // object add
            "c": [1, 9],           // array element update + removals
            "d": true              // key add
        });
        let s = diff_summary(&target, &base);
        // adds: b.y, d, and array shrink by 1 element => 2 adds? array shrink is removes, not adds.
        // target has shorter array than base -> removes count 1; array index 1 changed -> updates 1
        // scalar a changed -> updates += 1
        // new key d -> adds += 1; new key b.y -> adds += 1
        assert_eq!(s.adds, 2);
        assert_eq!(s.updates, 2);
        assert_eq!(s.removes, 1);
    }

    #[test]
    fn parse_yaml_errors_are_friendly() {
        // missing apiVersion
        let y1 = "kind: Foo\nmetadata:\n  name: x\n";
        let e1 = parse_yaml_for_target(y1, None).unwrap_err().to_string();
        assert!(e1.contains("missing apiVersion"), "e1={}", e1);

        // missing kind
        let y2 = "apiVersion: v1\nmetadata:\n  name: x\n";
        let e2 = parse_yaml_for_target(y2, None).unwrap_err().to_string();
        assert!(e2.contains("missing kind"), "e2={}", e2);

        // missing metadata.name
        let y3 = "apiVersion: v1\nkind: ConfigMap\nmetadata: {}\n";
        let e3 = parse_yaml_for_target(y3, None).unwrap_err().to_string();
        assert!(e3.contains("missing metadata.name"), "e3={}", e3);
    }
}
