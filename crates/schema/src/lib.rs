//! Orka schema (Milestone 1 stub): discover CRD schema, printer columns, and projected paths.

#![forbid(unsafe_code)]

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
// tracing optional here; keep code quiet for now
use orka_core::Projector;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrinterCol {
    pub name: String,
    pub json_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathSpec {
    pub id: u32,
    pub json_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchemaFlags {
    pub yaml_only_nodes: bool,
    pub preserves_unknown: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrdSchema {
    pub served_version: String,
    pub printer_cols: Vec<PrinterCol>,
    pub projected_paths: Vec<PathSpec>,
    pub flags: SchemaFlags,
}

fn normalize_json_path(jp: &str) -> Option<String> {
    // Accept only simple paths like .spec.foo.bar[0]
    if jp.contains('?') || jp.contains('*') { return None; }
    let s = if let Some(stripped) = jp.strip_prefix('.') { stripped } else { jp };
    if s.is_empty() { return None; }
    Some(s.to_string())
}

/// Try to fetch CRD schema for the provided `gvk_key` (e.g. "group/v1/Kind" or "v1/Kind").
/// Returns Ok(None) for built-in kinds without a CRD.
pub async fn fetch_crd_schema(gvk_key: &str) -> Result<Option<CrdSchema>> {
    use kube::Client;
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1 as apiextv1;
    use kube::{Api, api::ListParams};

    let client = Client::try_default().await?;
    // Parse key
    let parts: Vec<_> = gvk_key.split('/').collect();
    let (group, version, kind) = match parts.as_slice() {
        [version, kind] => ("", *version, *kind),
        [group, version, kind] => (*group, *version, *kind),
        _ => return Err(anyhow!("invalid gvk key: {}", gvk_key)),
    };

    if group.is_empty() {
        // Builtins have no CRD
        return Ok(None);
    }

    // List CRDs and find the one matching group + kind (robust across discovery quirks)
    let api: Api<apiextv1::CustomResourceDefinition> = Api::all(client.clone());
    let crds = api.list(&ListParams::default()).await.context("listing CustomResourceDefinitions")?;
    let mut v_opt: Option<serde_json::Value> = None;
    for crd in crds {
        let v = serde_json::to_value(&crd)?;
        let spec = match v.get("spec") { Some(s) => s, None => continue };
        let g = spec.get("group").and_then(|s| s.as_str()).unwrap_or("");
        let k = spec.get("names").and_then(|n| n.get("kind")).and_then(|s| s.as_str()).unwrap_or("");
        if g == group && k == kind { v_opt = Some(v); break; }
    }
    let v = match v_opt { Some(v) => v, None => return Err(anyhow!("CRD not found for {}", gvk_key)) };
    let versions = v
        .get("spec").and_then(|s| s.get("versions"))
        .and_then(|vv| vv.as_array())
        .cloned()
        .unwrap_or_default();

    // Pick a served version: prefer storage=true, else first served=true, else requested version
    let mut served_version = version.to_string();
    if !versions.is_empty() {
        if let Some(storage_v) = versions.iter().find(|ver| ver.get("storage").and_then(|b| b.as_bool()).unwrap_or(false)) {
            if let Some(name) = storage_v.get("name").and_then(|s| s.as_str()) { served_version = name.to_string(); }
        } else if let Some(served_v) = versions.iter().find(|ver| ver.get("served").and_then(|b| b.as_bool()).unwrap_or(false)) {
            if let Some(name) = served_v.get("name").and_then(|s| s.as_str()) { served_version = name.to_string(); }
        }
    }

    // Extract additionalPrinterColumns for the chosen version; fallback to top-level spec.additionalPrinterColumns (v1beta1 style)
    let mut printer_cols: Vec<PrinterCol> = Vec::new();
    if let Some(ver) = versions.iter().find(|ver| ver.get("name").and_then(|s| s.as_str()) == Some(served_version.as_str())) {
        if let Some(cols) = ver.get("additionalPrinterColumns").and_then(|c| c.as_array()) {
            for c in cols {
                let name = c.get("name").and_then(|s| s.as_str()).unwrap_or("").to_string();
                let raw = c.get("jsonPath").and_then(|s| s.as_str()).unwrap_or("");
                if !name.is_empty() {
                    if let Some(jp) = normalize_json_path(raw) {
                        printer_cols.push(PrinterCol { name, json_path: jp });
                    }
                }
            }
        }
    }
    if printer_cols.is_empty() {
        if let Some(cols) = v.get("spec").and_then(|s| s.get("additionalPrinterColumns")).and_then(|c| c.as_array()) {
            for c in cols {
                let name = c.get("name").and_then(|s| s.as_str()).unwrap_or("").to_string();
                let raw = c.get("jsonPath").and_then(|s| s.as_str()).unwrap_or("");
                if !name.is_empty() {
                    if let Some(jp) = normalize_json_path(raw) {
                        printer_cols.push(PrinterCol { name, json_path: jp });
                    }
                }
            }
        }
    }

    // Projected paths: prefer printer columns; else derive from OpenAPI schema
    let mut projected_paths: Vec<PathSpec> = Vec::new();
    if !printer_cols.is_empty() {
        for (i, c) in printer_cols.iter().enumerate() {
            projected_paths.push(PathSpec { id: i as u32, json_path: c.json_path.clone() });
            if projected_paths.len() >= 6 { break; }
        }
    } else {
        // Try to locate openAPIV3Schema for the chosen version
        let mut schema_opt: Option<&serde_json::Value> = None;
        if let Some(ver) = versions.iter().find(|ver| ver.get("name").and_then(|s| s.as_str()) == Some(served_version.as_str())) {
            schema_opt = ver.get("schema").and_then(|s| s.get("openAPIV3Schema"));
        }
        if schema_opt.is_none() {
            // legacy v1beta1 location
            schema_opt = v.get("spec").and_then(|s| s.get("validation")).and_then(|s| s.get("openAPIV3Schema"));
        }

        let candidates = schema_opt
            .and_then(|s| derive_projected_from_openapi(s))
            .unwrap_or_else(|| vec![
                "spec.name".to_string(),
                "spec.namespace".to_string(),
            ]);
        for (i, p) in candidates.iter().take(6).enumerate() {
            projected_paths.push(PathSpec { id: i as u32, json_path: p.clone() });
        }
    }

    Ok(Some(CrdSchema { served_version, printer_cols, projected_paths, flags: SchemaFlags::default() }))
}

/// Simple projector built from a `CrdSchema` projected paths.
#[derive(Clone)]
pub struct SchemaProjector {
    specs: Vec<PathSpec>,
}

impl SchemaProjector {
    pub fn new(specs: Vec<PathSpec>) -> Self { Self { specs } }

    /// Extract a scalar string from a JSON value following a minimal json-path-like grammar:
    /// dot fields and single `[index]` on a segment, e.g., `spec.dnsNames[0]`.
    fn extract_path<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
        use serde_json::Value;
        let mut cur = root;
        for seg in path.split('.') {
            if seg.is_empty() { return None; }
            // Handle optional [index]
            let (key, idx_opt) = if let Some(brk) = seg.find('[') {
                let end = seg.get(brk+1..)?.find(']')? + brk + 1;
                let key = &seg[..brk];
                let idx_str = &seg[brk+1..end];
                let idx: usize = idx_str.parse().ok()?;
                (key, Some(idx))
            } else {
                (seg, None)
            };
            match cur {
                Value::Object(map) => {
                    cur = map.get(key)?;
                }
                _ => return None,
            }
            if let Some(i) = idx_opt {
                match cur {
                    Value::Array(arr) => { cur = arr.get(i)?; }
                    _ => return None,
                }
            }
        }
        Some(cur)
    }
}

impl Projector for SchemaProjector {
    fn project(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out: SmallVec<[(u32, String); 8]> = SmallVec::new();
        for spec in self.specs.iter() {
            if let Some(v) = Self::extract_path(raw, &spec.json_path) {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    _ => continue,
                };
                out.push((spec.id, s));
                if out.len() >= 8 { break; }
            }
        }
        out
    }
}

impl CrdSchema {
    pub fn projector(&self) -> SchemaProjector {
        SchemaProjector::new(self.projected_paths.clone())
    }
}

fn derive_projected_from_openapi(schema: &serde_json::Value) -> Option<Vec<String>> {
    use serde_json::Value;
    let mut out: Vec<String> = Vec::new();
    let spec_props = schema.get("properties")?.get("spec")?.get("properties")?.as_object()?;

    fn is_scalar_type(ty: &str) -> bool { matches!(ty, "string" | "integer" | "number" | "boolean") }

    fn walk_object(obj: &serde_json::Map<String, Value>, base: &str, depth: usize, out: &mut Vec<String>) {
        if depth > 3 || out.len() >= 16 { return; }
        for (k, v) in obj.iter() {
            let path = if base.is_empty() { k.clone() } else { format!("{}.{}", base, k) };
            let ty = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
            match ty {
                "object" => {
                    if let Some(props) = v.get("properties").and_then(|p| p.as_object()) {
                        walk_object(props, &path, depth+1, out);
                    }
                }
                "array" => {
                    if let Some(items) = v.get("items") {
                        let ity = items.get("type").and_then(|s| s.as_str()).unwrap_or("");
                        if is_scalar_type(ity) {
                            out.push(format!("{}[0]", path));
                        } else if ity == "object" {
                            if let Some(props) = items.get("properties").and_then(|p| p.as_object()) {
                                walk_object(props, &format!("{}[0]", path), depth+1, out);
                            }
                        }
                    }
                }
                t if is_scalar_type(t) => {
                    out.push(path);
                }
                _ => {}
            }
            if out.len() >= 16 { return; }
        }
    }

    walk_object(spec_props, "spec", 0, &mut out);

    if out.is_empty() { None } else { Some(out) }
}
