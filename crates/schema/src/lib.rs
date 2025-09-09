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
    // Validate segments: allow alnum/underscore/hyphen keys; optional single [index] at end
    for seg in s.split('.') {
        if seg.is_empty() { return None; }
        let bytes = seg.as_bytes();
        // count '[' occurrences
        let mut open_idx: Option<usize> = None;
        for (i, ch) in bytes.iter().enumerate() {
            match *ch as char {
                '[' => {
                    if open_idx.is_some() { return None; } // multiple [
                    open_idx = Some(i);
                }
                ']' => {
                    // ']' only allowed if we saw '[' and it must be the last char
                    match open_idx {
                        Some(start) => {
                            if i != bytes.len() - 1 { return None; }
                            // ensure digits between [ and ]
                            if start + 1 >= i { return None; }
                            if !seg[start+1..i].chars().all(|c| c.is_ascii_digit()) { return None; }
                        }
                        None => return None,
                    }
                }
                c => {
                    // before '[' ensure key chars are safe
                    if open_idx.is_none() {
                        if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') { return None; }
                    } else {
                        // inside index; digits validated on closing
                    }
                }
            }
        }
        // if '[' opened, we must have seen a closing ']' (enforced above by requiring it as last char)
        if let Some(start) = open_idx {
            if !seg.ends_with(']') || start >= seg.len()-1 { return None; }
        }
    }
    Some(s.to_string())
}

/// Try to fetch CRD schema for the provided `gvk_key` (e.g. "group/v1/Kind" or "v1/Kind").
/// Returns Ok(None) for built-in kinds without a CRD.
pub async fn fetch_crd_schema(gvk_key: &str) -> Result<Option<CrdSchema>> {
    use kube::Client;
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1 as apiextv1;
    use kube::{Api, api::ListParams};

    let client = orka_kubehub::get_kube_client().await?;
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

// Feature-gated JSON Schema validation utilities
#[cfg(feature = "jsonschema-validate")]
pub mod validate {
    use super::*;
    use anyhow::{Context, Result};
    use jsonschema::{Draft, JSONSchema};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ValidationIssue {
        pub path: String,
        pub error: String,
        pub hint: Option<String>,
    }

    async fn fetch_openapi_schema(gvk_key: &str) -> Result<Option<serde_json::Value>> {
        use kube::Client;
        use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1 as apiextv1;
        use kube::{Api, api::ListParams};

        let client = orka_kubehub::get_kube_client().await?;
        let parts: Vec<_> = gvk_key.split('/').collect();
        let (group, version, kind) = match parts.as_slice() {
            [version, kind] => ("", *version, *kind),
            [group, version, kind] => (*group, *version, *kind),
            _ => return Err(anyhow::anyhow!("invalid gvk key: {}", gvk_key)),
        };
        if group.is_empty() { return Ok(None); }

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
        let v = match v_opt { Some(v) => v, None => return Err(anyhow::anyhow!("CRD not found for {}", gvk_key)) };
        let versions = v.get("spec").and_then(|s| s.get("versions")).and_then(|vv| vv.as_array()).cloned().unwrap_or_default();
        let mut served_version = version.to_string();
        if !versions.is_empty() {
            if let Some(storage_v) = versions.iter().find(|ver| ver.get("storage").and_then(|b| b.as_bool()).unwrap_or(false)) {
                if let Some(name) = storage_v.get("name").and_then(|s| s.as_str()) { served_version = name.to_string(); }
            } else if let Some(served_v) = versions.iter().find(|ver| ver.get("served").and_then(|b| b.as_bool()).unwrap_or(false)) {
                if let Some(name) = served_v.get("name").and_then(|s| s.as_str()) { served_version = name.to_string(); }
            }
        }
        // Find openAPIV3Schema in chosen version or legacy location
        let mut schema_opt: Option<serde_json::Value> = None;
        if let Some(ver) = versions.iter().find(|ver| ver.get("name").and_then(|s| s.as_str()) == Some(served_version.as_str())) {
            if let Some(s) = ver.get("schema").and_then(|s| s.get("openAPIV3Schema")).cloned() { schema_opt = Some(s); }
        }
        if schema_opt.is_none() {
            schema_opt = v.get("spec").and_then(|s| s.get("validation")).and_then(|s| s.get("openAPIV3Schema")).cloned();
        }
        Ok(schema_opt)
    }

    /// Validate a YAML document against the CRD's `openAPIV3Schema` for the given GVK.
    /// Returns a list of human-friendly issues; empty on success.
    pub async fn validate_yaml_for_gvk(gvk_key: &str, yaml: &str) -> Result<Vec<ValidationIssue>> {
        let schema = match fetch_openapi_schema(gvk_key).await? {
            Some(s) => s,
            None => return Ok(vec![ValidationIssue { path: "".into(), error: "no CRD schema available for builtin kind".into(), hint: None }]),
        };
        let json: serde_json::Value = match serde_yaml::from_str::<serde_yaml::Value>(yaml) {
            Ok(v) => serde_json::to_value(v).context("converting YAML to JSON")?,
            Err(e) => return Ok(vec![ValidationIssue { path: "".into(), error: format!("YAML parse error: {}", e), hint: Some("check indentation and syntax".into()) }]),
        };
        // JSONSchema 0.17 requires a 'static schema reference; leak for now (acceptable for CLI usage).
        let schema_static: &'static serde_json::Value = Box::leak(Box::new(schema));
        let compiled = JSONSchema::options().with_draft(Draft::Draft7).compile(schema_static).context("compiling CRD JSON Schema")?;
        let mut issues: Vec<ValidationIssue> = Vec::new();
        let result = compiled.validate(&json);
        if let Err(errors) = result {
            for err in errors {
                let path = err.instance_path.to_string();
                let error = err.to_string();
                // Keep hints minimal to avoid depending on specific jsonschema internals
                let hint = if error.contains("required property") {
                    Some("missing required field".into())
                } else if error.contains("type:") || error.contains("expected type") {
                    Some("mismatched type".into())
                } else if error.contains("enum") {
                    Some("value not in allowed set".into())
                } else {
                    None
                };
                issues.push(ValidationIssue { path, error, hint });
            }
        }
        Ok(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_json_path_accepts_simple_paths() {
        assert_eq!(normalize_json_path(".spec.foo"), Some("spec.foo".to_string()));
        assert_eq!(normalize_json_path("spec.dnsNames[0]"), Some("spec.dnsNames[0]".to_string()));
        assert_eq!(normalize_json_path("").is_none(), true);
        assert_eq!(normalize_json_path("spec.*").is_none(), true);
        assert_eq!(normalize_json_path("spec.foo[0][1]").is_some(), false);
    }

    #[test]
    fn projector_extracts_scalars() {
        let json = serde_json::json!({
            "spec": {
                "dnsNames": ["a.example.com", "b.example.com"],
                "replicas": 3,
                "paused": false
            }
        });
        let specs = vec![
            PathSpec { id: 1, json_path: "spec.dnsNames[0]".to_string() },
            PathSpec { id: 2, json_path: "spec.replicas".to_string() },
            PathSpec { id: 3, json_path: "spec.paused".to_string() },
        ];
        let pj = SchemaProjector::new(specs);
        let out = pj.project(&json);
        assert!(out.contains(&(1, "a.example.com".to_string())));
        assert!(out.contains(&(2, "3".to_string())));
        assert!(out.contains(&(3, "false".to_string())));
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
