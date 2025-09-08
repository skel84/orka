//! Built-in columns and projectors for core Kubernetes kinds.
//!
//! This module provides:
//! - Stable column IDs + specs (labels, widths, kinds)
//! - A simple registry mapping G/V/K to column sets
//! - A JSON projector for built-ins that fills `LiteObj.projected`

#![forbid(unsafe_code)]

use smallvec::SmallVec;

use crate::Projector;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnKind {
    Namespace,
    Name,
    Age,
    Projected(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSpec {
    pub kind: ColumnKind,
    pub label: &'static str,
    pub width: f32,
}

// ---------------- Column IDs (stable) ----------------
// Pods
pub const POD_READY: u32 = 10_001;
pub const POD_STATUS: u32 = 10_002;
pub const POD_RESTARTS: u32 = 10_003;
pub const POD_NODE: u32 = 10_004;

// Deployments
pub const DEP_READY: u32 = 11_001;
pub const DEP_UPDATED: u32 = 11_002;
pub const DEP_AVAILABLE: u32 = 11_003;

// StatefulSets
pub const STS_READY: u32 = 12_001;

// Services
pub const SVC_TYPE: u32 = 13_001;
pub const SVC_CLUSTER_IP: u32 = 13_002;
pub const SVC_EXTERNAL_IP: u32 = 13_003;
pub const SVC_PORTS: u32 = 13_004;

// Ingress
pub const ING_CLASS: u32 = 14_001;
pub const ING_HOSTS: u32 = 14_002;
pub const ING_ADDRESS: u32 = 14_003;
pub const ING_TLS: u32 = 14_004;

// DaemonSets
pub const DS_DESIRED: u32 = 15_001;
pub const DS_CURRENT: u32 = 15_002;
pub const DS_READY: u32 = 15_003;
pub const DS_UPDATED: u32 = 15_004;
pub const DS_AVAILABLE: u32 = 15_005;

// Jobs
pub const JOB_COMPLETIONS: u32 = 16_001;
pub const JOB_STATUS: u32 = 16_002;

// CronJobs
pub const CJ_SCHEDULE: u32 = 17_001;
pub const CJ_SUSPEND: u32 = 17_002;
pub const CJ_ACTIVE: u32 = 17_003;
pub const CJ_LAST_SCHEDULE: u32 = 17_004;

// PVCs
pub const PVC_STATUS: u32 = 18_001;
pub const PVC_VOLUME: u32 = 18_002;
pub const PVC_CAPACITY: u32 = 18_003;
pub const PVC_ACCESS_MODES: u32 = 18_004;
pub const PVC_STORAGECLASS: u32 = 18_005;

// Nodes
pub const NODE_STATUS: u32 = 19_001;
pub const NODE_ROLES: u32 = 19_002;
pub const NODE_VERSION: u32 = 19_003;

// Namespaces
pub const NS_STATUS: u32 = 20_001;

fn col(kind: ColumnKind, label: &'static str, width: f32) -> ColumnSpec {
    ColumnSpec { kind, label, width }
}

/// Return full column set for a built-in kind, including Namespace/Name/Age.
/// Fallback to just Namespace/Name/Age when no opinionated columns are known.
pub fn builtin_columns_for(group: &str, version: &str, kind: &str, namespaced: bool) -> Vec<ColumnSpec> {
    let mut cols: Vec<ColumnSpec> = Vec::new();
    if namespaced {
        cols.push(col(ColumnKind::Namespace, "Namespace", 160.0));
    }
    cols.push(col(ColumnKind::Name, "Name", 240.0));

    match (group, version, kind) {
        ("", "v1", "Pod") => {
            cols.push(col(ColumnKind::Projected(POD_READY), "Ready", 80.0));
            cols.push(col(ColumnKind::Projected(POD_STATUS), "Status", 100.0));
            cols.push(col(ColumnKind::Projected(POD_RESTARTS), "Restarts", 80.0));
            cols.push(col(ColumnKind::Projected(POD_NODE), "Node", 140.0));
        }
        ("apps", "v1", "Deployment") => {
            cols.push(col(ColumnKind::Projected(DEP_READY), "Ready", 90.0));
            cols.push(col(ColumnKind::Projected(DEP_UPDATED), "Up-to-date", 90.0));
            cols.push(col(ColumnKind::Projected(DEP_AVAILABLE), "Available", 90.0));
        }
        ("apps", "v1", "StatefulSet") => {
            cols.push(col(ColumnKind::Projected(STS_READY), "Ready", 90.0));
        }
        ("apps", "v1", "DaemonSet") => {
            cols.push(col(ColumnKind::Projected(DS_DESIRED), "Desired", 80.0));
            cols.push(col(ColumnKind::Projected(DS_CURRENT), "Current", 80.0));
            cols.push(col(ColumnKind::Projected(DS_READY), "Ready", 80.0));
            cols.push(col(ColumnKind::Projected(DS_UPDATED), "Up-to-date", 90.0));
            cols.push(col(ColumnKind::Projected(DS_AVAILABLE), "Available", 90.0));
        }
        ("", "v1", "Service") => {
            cols.push(col(ColumnKind::Projected(SVC_TYPE), "Type", 80.0));
            cols.push(col(ColumnKind::Projected(SVC_CLUSTER_IP), "Cluster IP", 120.0));
            cols.push(col(ColumnKind::Projected(SVC_EXTERNAL_IP), "External IP", 160.0));
            cols.push(col(ColumnKind::Projected(SVC_PORTS), "Ports", 140.0));
        }
        ("networking.k8s.io", "v1", "Ingress") => {
            cols.push(col(ColumnKind::Projected(ING_CLASS), "Class", 100.0));
            cols.push(col(ColumnKind::Projected(ING_HOSTS), "Hosts", 160.0));
            cols.push(col(ColumnKind::Projected(ING_ADDRESS), "Address", 160.0));
            cols.push(col(ColumnKind::Projected(ING_TLS), "TLS", 50.0));
        }
        ("batch", "v1", "Job") => {
            cols.push(col(ColumnKind::Projected(JOB_COMPLETIONS), "Completions", 100.0));
            cols.push(col(ColumnKind::Projected(JOB_STATUS), "Status", 100.0));
        }
        ("batch", "v1", "CronJob") => {
            cols.push(col(ColumnKind::Projected(CJ_SCHEDULE), "Schedule", 120.0));
            cols.push(col(ColumnKind::Projected(CJ_SUSPEND), "Suspend", 80.0));
            cols.push(col(ColumnKind::Projected(CJ_ACTIVE), "Active", 70.0));
            cols.push(col(ColumnKind::Projected(CJ_LAST_SCHEDULE), "Last Schedule", 140.0));
        }
        ("", "v1", "PersistentVolumeClaim") => {
            cols.push(col(ColumnKind::Projected(PVC_STATUS), "Status", 90.0));
            cols.push(col(ColumnKind::Projected(PVC_VOLUME), "Volume", 120.0));
            cols.push(col(ColumnKind::Projected(PVC_CAPACITY), "Capacity", 90.0));
            cols.push(col(ColumnKind::Projected(PVC_ACCESS_MODES), "Access Modes", 130.0));
            cols.push(col(ColumnKind::Projected(PVC_STORAGECLASS), "StorageClass", 120.0));
        }
        ("", "v1", "Namespace") => {
            // cluster-scoped: Name, Status, Age
            cols.push(col(ColumnKind::Projected(NS_STATUS), "Status", 90.0));
        }
        ("", "v1", "Node") => {
            // cluster-scoped: Name, Status, Roles, Version, Age
            cols.push(col(ColumnKind::Projected(NODE_STATUS), "Status", 90.0));
            cols.push(col(ColumnKind::Projected(NODE_ROLES), "Roles", 120.0));
            cols.push(col(ColumnKind::Projected(NODE_VERSION), "Version", 110.0));
        }
        _ => {}
    }

    cols.push(col(ColumnKind::Age, "Age", 70.0));
    cols
}

fn gvk_key(group: &str, version: &str, kind: &str) -> String {
    if group.is_empty() { format!("{}/{}", version, kind) } else { format!("{}/{}/{}", group, version, kind) }
}

/// Return a JSON projector for a supported built-in kind.
pub fn builtin_projector_for(group: &str, version: &str, kind: &str) -> Option<std::sync::Arc<dyn Projector + Send + Sync>> {
    let key = gvk_key(group, version, kind);
    match key.as_str() {
        "v1/Pod" | "apps/v1/Deployment" | "apps/v1/StatefulSet" | "apps/v1/DaemonSet" |
        "v1/Service" | "networking.k8s.io/v1/Ingress" | "batch/v1/Job" | "batch/v1/CronJob" |
        "v1/PersistentVolumeClaim" | "v1/Node" | "v1/Namespace" => {
            Some(std::sync::Arc::new(BuiltinProjector { gvk_key: key }))
        }
        _ => None,
    }
}

struct BuiltinProjector {
    gvk_key: String,
}

impl BuiltinProjector {
    fn project_pod(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        // Ready X/Y
        let mut ready = 0u32; let mut total = 0u32; let mut restarts = 0u32;
        if let Some(cs) = raw.pointer("/status/containerStatuses").and_then(|v| v.as_array()) {
            total = cs.len() as u32;
            for c in cs {
                if c.get("ready").and_then(|v| v.as_bool()).unwrap_or(false) { ready += 1; }
                restarts += c.get("restartCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            }
        }
        out.push((POD_READY, format!("{}/{}", ready, total)));
        out.push((POD_RESTARTS, restarts.to_string()));
        // Status (phase or reason)
        let phase = raw.pointer("/status/phase").and_then(|v| v.as_str()).unwrap_or("");
        let reason = raw.pointer("/status/reason").and_then(|v| v.as_str()).unwrap_or("");
        let status = if !reason.is_empty() { reason.to_string() } else { phase.to_string() };
        if !status.is_empty() { out.push((POD_STATUS, status)); }
        // Node name
        if let Some(node) = raw.pointer("/spec/nodeName").and_then(|v| v.as_str()) {
            out.push((POD_NODE, node.to_string()));
        }
        out
    }

    fn project_deployment(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        let replicas = raw.pointer("/status/replicas").and_then(|v| v.as_u64()).unwrap_or(0);
        let ready = raw.pointer("/status/readyReplicas").and_then(|v| v.as_u64()).unwrap_or(0);
        let updated = raw.pointer("/status/updatedReplicas").and_then(|v| v.as_u64()).unwrap_or(0);
        let available = raw.pointer("/status/availableReplicas").and_then(|v| v.as_u64()).unwrap_or(0);
        out.push((DEP_READY, format!("{}/{}", ready, replicas)));
        out.push((DEP_UPDATED, updated.to_string()));
        out.push((DEP_AVAILABLE, available.to_string()));
        out
    }

    fn project_statefulset(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        let replicas = raw.pointer("/status/replicas").and_then(|v| v.as_u64()).unwrap_or(0);
        let ready = raw.pointer("/status/readyReplicas").and_then(|v| v.as_u64()).unwrap_or(0);
        out.push((STS_READY, format!("{}/{}", ready, replicas)));
        out
    }

    fn project_service(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        if let Some(t) = raw.pointer("/spec/type").and_then(|v| v.as_str()) {
            out.push((SVC_TYPE, t.to_string()));
        }
        if let Some(ip) = raw.pointer("/spec/clusterIP").and_then(|v| v.as_str()) {
            out.push((SVC_CLUSTER_IP, ip.to_string()));
        }
        // External IPs from spec.externalIPs or status.loadBalancer.ingress
        let mut eps: Vec<String> = Vec::new();
        if let Some(arr) = raw.pointer("/spec/externalIPs").and_then(|v| v.as_array()) {
            for it in arr { if let Some(s) = it.as_str() { eps.push(s.to_string()); } }
        }
        if eps.is_empty() {
            if let Some(arr) = raw.pointer("/status/loadBalancer/ingress").and_then(|v| v.as_array()) {
                for it in arr {
                    if let Some(ip) = it.get("ip").and_then(|v| v.as_str()) { eps.push(ip.to_string()); }
                    else if let Some(h) = it.get("hostname").and_then(|v| v.as_str()) { eps.push(h.to_string()); }
                }
            }
        }
        if !eps.is_empty() { out.push((SVC_EXTERNAL_IP, eps.join(","))); }
        // Ports
        if let Some(ports) = raw.pointer("/spec/ports").and_then(|v| v.as_array()) {
            let mut v: Vec<String> = Vec::new();
            for p in ports.iter().take(4) {
                let port = p.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
                let proto = p.get("protocol").and_then(|v| v.as_str()).unwrap_or("TCP");
                if let Some(name) = p.get("name").and_then(|v| v.as_str()) {
                    v.push(format!("{}:{}", name, port));
                } else {
                    v.push(format!("{}/{}", port, proto));
                }
            }
            if !v.is_empty() { out.push((SVC_PORTS, v.join(","))); }
        }
        out
    }

    fn project_ingress(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        if let Some(c) = raw.pointer("/spec/ingressClassName").and_then(|v| v.as_str()) {
            out.push((ING_CLASS, c.to_string()));
        }
        // Hosts
        if let Some(rules) = raw.pointer("/spec/rules").and_then(|v| v.as_array()) {
            let mut hosts: Vec<String> = Vec::new();
            for r in rules { if let Some(h) = r.get("host").and_then(|v| v.as_str()) { hosts.push(h.to_string()); } }
            if !hosts.is_empty() { out.push((ING_HOSTS, hosts.join(","))); }
        }
        // Address
        if let Some(arr) = raw.pointer("/status/loadBalancer/ingress").and_then(|v| v.as_array()) {
            let mut addrs: Vec<String> = Vec::new();
            for it in arr {
                if let Some(ip) = it.get("ip").and_then(|v| v.as_str()) { addrs.push(ip.to_string()); }
                else if let Some(h) = it.get("hostname").and_then(|v| v.as_str()) { addrs.push(h.to_string()); }
            }
            if !addrs.is_empty() { out.push((ING_ADDRESS, addrs.join(","))); }
        }
        // TLS
        if let Some(tls) = raw.pointer("/spec/tls").and_then(|v| v.as_array()) {
            if !tls.is_empty() { out.push((ING_TLS, "Y".to_string())); } else { out.push((ING_TLS, "N".to_string())); }
        }
        out
    }

    fn project_daemonset(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        let desired = raw.pointer("/status/desiredNumberScheduled").and_then(|v| v.as_u64()).unwrap_or(0);
        let current = raw.pointer("/status/currentNumberScheduled").and_then(|v| v.as_u64()).unwrap_or(0);
        let ready = raw.pointer("/status/numberReady").and_then(|v| v.as_u64()).unwrap_or(0);
        let updated = raw.pointer("/status/updatedNumberScheduled").and_then(|v| v.as_u64()).unwrap_or(0);
        let available = raw.pointer("/status/numberAvailable").and_then(|v| v.as_u64()).unwrap_or(0);
        out.push((DS_DESIRED, desired.to_string()));
        out.push((DS_CURRENT, current.to_string()));
        out.push((DS_READY, ready.to_string()));
        out.push((DS_UPDATED, updated.to_string()));
        out.push((DS_AVAILABLE, available.to_string()));
        out
    }

    fn project_job(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        let desired = raw.pointer("/spec/completions").and_then(|v| v.as_u64()).unwrap_or(1);
        let succeeded = raw.pointer("/status/succeeded").and_then(|v| v.as_u64()).unwrap_or(0);
        out.push((JOB_COMPLETIONS, format!("{}/{}", succeeded, desired)));
        // Status: Complete/Failed/Active
        let mut status = String::new();
        if let Some(conds) = raw.pointer("/status/conditions").and_then(|v| v.as_array()) {
            for c in conds {
                let t = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let s = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if t == "Complete" && s == "True" { status = "Complete".into(); break; }
                if t == "Failed" && s == "True" { status = "Failed".into(); }
            }
        }
        if status.is_empty() {
            let active = raw.pointer("/status/active").and_then(|v| v.as_u64()).unwrap_or(0);
            if active > 0 { status = format!("Active ({})", active); }
        }
        if status.is_empty() { status = "-".into(); }
        out.push((JOB_STATUS, status));
        out
    }

    fn project_cronjob(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        if let Some(s) = raw.pointer("/spec/schedule").and_then(|v| v.as_str()) { out.push((CJ_SCHEDULE, s.to_string())); }
        if let Some(b) = raw.pointer("/spec/suspend").and_then(|v| v.as_bool()) { out.push((CJ_SUSPEND, if b { "True".into() } else { "False".into() })); }
        let active = raw.pointer("/status/active").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        out.push((CJ_ACTIVE, active.to_string()));
        if let Some(ts) = raw.pointer("/status/lastScheduleTime").and_then(|v| v.as_str()) { out.push((CJ_LAST_SCHEDULE, ts.to_string())); }
        out
    }

    fn project_pvc(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        if let Some(s) = raw.pointer("/status/phase").and_then(|v| v.as_str()) { out.push((PVC_STATUS, s.to_string())); }
        if let Some(v) = raw.pointer("/spec/volumeName").and_then(|v| v.as_str()) { out.push((PVC_VOLUME, v.to_string())); }
        if let Some(cap) = raw.pointer("/status/capacity/storage").and_then(|v| v.as_str()) { out.push((PVC_CAPACITY, cap.to_string())); }
        if let Some(modes) = raw.pointer("/spec/accessModes").and_then(|v| v.as_array()) {
            let vals: Vec<String> = modes.iter().filter_map(|m| m.as_str().map(|s| s.to_string())).collect();
            if !vals.is_empty() { out.push((PVC_ACCESS_MODES, vals.join(","))); }
        }
        if let Some(sc) = raw.pointer("/spec/storageClassName").and_then(|v| v.as_str()) { out.push((PVC_STORAGECLASS, sc.to_string())); }
        out
    }

    fn project_node(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        // Status from conditions Ready
        let mut status = "Unknown".to_string();
        if let Some(conds) = raw.pointer("/status/conditions").and_then(|v| v.as_array()) {
            for c in conds {
                if c.get("type").and_then(|v| v.as_str()) == Some("Ready") {
                    status = if c.get("status").and_then(|v| v.as_str()) == Some("True") { "Ready".into() } else { "NotReady".into() };
                    break;
                }
            }
        }
        out.push((NODE_STATUS, status));
        // Roles from labels
        let mut roles: Vec<String> = Vec::new();
        if let Some(lbls) = raw.pointer("/metadata/labels").and_then(|v| v.as_object()) {
            for (k, v) in lbls.iter() {
                if k.starts_with("node-role.kubernetes.io/") {
                    let role = k.trim_start_matches("node-role.kubernetes.io/");
                    roles.push(if role.is_empty() { "node".into() } else { role.to_string() });
                }
            }
            if roles.is_empty() {
                if let Some(r) = lbls.get("kubernetes.io/role").and_then(|v| v.as_str()) { roles.push(r.to_string()); }
            }
        }
        if roles.is_empty() { roles.push("none".into()); }
        out.push((NODE_ROLES, roles.join(",")));
        if let Some(v) = raw.pointer("/status/nodeInfo/kubeletVersion").and_then(|v| v.as_str()) { out.push((NODE_VERSION, v.to_string())); }
        out
    }

    fn project_namespace(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        let mut out = SmallVec::new();
        if let Some(s) = raw.pointer("/status/phase").and_then(|v| v.as_str()) { out.push((NS_STATUS, s.to_string())); }
        out
    }
}

impl Projector for BuiltinProjector {
    fn project(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]> {
        match self.gvk_key.as_str() {
            "v1/Pod" => self.project_pod(raw),
            "apps/v1/Deployment" => self.project_deployment(raw),
            "apps/v1/StatefulSet" => self.project_statefulset(raw),
            "apps/v1/DaemonSet" => self.project_daemonset(raw),
            "v1/Service" => self.project_service(raw),
            "networking.k8s.io/v1/Ingress" => self.project_ingress(raw),
            "batch/v1/Job" => self.project_job(raw),
            "batch/v1/CronJob" => self.project_cronjob(raw),
            "v1/PersistentVolumeClaim" => self.project_pvc(raw),
            "v1/Node" => self.project_node(raw),
            "v1/Namespace" => self.project_namespace(raw),
            _ => SmallVec::new(),
        }
    }
}
