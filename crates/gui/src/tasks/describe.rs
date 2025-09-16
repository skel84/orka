#![forbid(unsafe_code)]

use crate::{OrkaGuiApp, UiUpdate};
use kube::{api::Api, core::DynamicObject};
use orka_api::ResourceRef;
use orka_core::Uid;
use tracing::info;

impl OrkaGuiApp {
    fn ensure_updates_channel_for_describe(&mut self) -> std::sync::mpsc::Sender<UiUpdate> {
        if let Some(tx) = &self.watch.updates_tx {
            return tx.clone();
        }
        let (tx, rx) = std::sync::mpsc::channel::<UiUpdate>();
        self.watch.updates_tx = Some(tx.clone());
        self.watch.updates_rx = Some(rx);
        tx
    }

    pub(crate) fn start_describe_task(&mut self, uid: Uid) {
        // cancel previous describe task if any
        if let Some(task) = self.describe.task.take() {
            task.abort();
        }
        if let Some(stop) = self.describe.stop.take() {
            let _ = stop.send(());
        }
        self.describe.running = true;
        self.describe.text.clear();
        self.describe.error = None;
        self.describe.uid = Some(uid);

        // Resolve GVK + ns/name for kubectl
        let (gvk_opt, ns_opt, name_opt) = if let Some(i) = self.results.index.get(&uid).copied() {
            if let Some(row) = self.results.rows.get(i) {
                (
                    self.current_selected_kind().cloned(),
                    row.namespace.clone(),
                    Some(row.name.clone()),
                )
            } else {
                (self.current_selected_kind().cloned(), None, None)
            }
        } else {
            (self.current_selected_kind().cloned(), None, None)
        };
        let gvk = match gvk_opt {
            Some(k) => k,
            None => {
                self.describe.running = false;
                return;
            }
        };
        let name = match name_opt {
            Some(n) => n,
            None => {
                self.describe.running = false;
                return;
            }
        };
        let tx = self.ensure_updates_channel_for_describe();
        let api = self.api.clone();
        let reference = ResourceRef {
            cluster: None,
            gvk: gvk.clone(),
            namespace: ns_opt.clone(),
            name: name.clone(),
        };
        self.describe.task = Some(tokio::spawn(async move {
            match api.get_raw(reference).await {
                Ok(bytes) => {
                    let (text, ns_str, uid_str, gvk_owned) =
                        match serde_json::from_slice::<serde_json::Value>(&bytes) {
                            Ok(v) => {
                                let base = render_describe(&v);
                                let ns_str = v
                                    .get("metadata")
                                    .and_then(|m| m.get("namespace"))
                                    .and_then(|x| x.as_str())
                                    .map(|s| s.to_string());
                                let uid_str = v
                                    .get("metadata")
                                    .and_then(|m| m.get("uid"))
                                    .and_then(|x| x.as_str())
                                    .map(|s| s.to_string());
                                let gvk =
                                    v.get("apiVersion").and_then(|x| x.as_str()).unwrap_or("");
                                let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                                (base, ns_str, uid_str, format!("{}/{}", gvk, kind))
                            }
                            Err(_) => (
                                String::from_utf8_lossy(&bytes).into_owned(),
                                None,
                                None,
                                String::new(),
                            ),
                        };
                    // Try to fetch events and append
                    let events_text = fetch_events_for(&ns_str, &uid_str, &gvk_owned).await;
                    // Always show Events section; leave it empty if there are none
                    let final_text = format!("{}\n\nEvents:\n{}", text, events_text);
                    let _ = tx.send(UiUpdate::DescribeReady {
                        uid,
                        text: final_text,
                    });
                }
                Err(e) => {
                    let _ = tx.send(UiUpdate::DescribeError {
                        uid,
                        error: e.to_string(),
                    });
                }
            }
            info!("describe: task ended");
        }));
    }
}

fn render_describe(v: &serde_json::Value) -> String {
    let kind = v.get("kind").and_then(|s| s.as_str()).unwrap_or("");
    match kind {
        "Pod" => describe_pod(v),
        "Deployment" => describe_deployment(v),
        "ReplicaSet" => describe_replicaset(v),
        "StatefulSet" => describe_statefulset(v),
        "DaemonSet" => describe_daemonset(v),
        "Job" => describe_job(v),
        "CronJob" => describe_cronjob(v),
        "Service" => describe_service(v),
        "Ingress" | "IngressClass" => describe_ingress(v),
        "ConfigMap" => describe_configmap(v),
        "Secret" => describe_secret(v),
        "PersistentVolumeClaim" => describe_pvc(v),
        "Namespace" => describe_namespace(v),
        "Node" => describe_node(v),
        _ => describe_generic(v),
    }
}

fn push_kv(out: &mut String, k: &str, v: impl AsRef<str>) {
    let _ = std::fmt::write(out, format_args!("{k}: {}\n", v.as_ref()));
}

fn kv<'a>(v: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut cur = v;
    for p in path {
        cur = cur.get(*p)?;
    }
    Some(cur)
}

fn kvs(v: &serde_json::Value, path: &[&str]) -> Option<String> {
    kv(v, path).and_then(|x| x.as_str().map(|s| s.to_string()))
}

fn describe_generic(v: &serde_json::Value) -> String {
    let mut out = String::new();
    let kind = v
        .get("kind")
        .and_then(|s| s.as_str())
        .unwrap_or("(unknown)");
    let name = kvs(v, &["metadata", "name"]).unwrap_or_default();
    let ns = kvs(v, &["metadata", "namespace"]).unwrap_or_else(|| "-".into());
    push_kv(&mut out, "Kind", kind);
    push_kv(&mut out, "Namespace", ns);
    push_kv(&mut out, "Name", name);
    if let Some(owners) = kv(v, &["metadata", "ownerReferences"]).and_then(|x| x.as_array()) {
        if !owners.is_empty() {
            out.push_str("Owner References:\n");
            for o in owners {
                let ok = o.get("kind").and_then(|s| s.as_str()).unwrap_or("");
                let on = o.get("name").and_then(|s| s.as_str()).unwrap_or("");
                push_kv(&mut out, &format!("  {}", ok), on);
            }
        }
    }
    if let Some(labels) = kv(v, &["metadata", "labels"]).and_then(|x| x.as_object()) {
        out.push_str("Labels:\n");
        for (k, v) in labels {
            push_kv(&mut out, &format!("  {}", k), v.as_str().unwrap_or(""));
        }
    }
    if let Some(annos) = kv(v, &["metadata", "annotations"]).and_then(|x| x.as_object()) {
        out.push_str("Annotations:\n");
        for (k, v) in annos {
            push_kv(&mut out, &format!("  {}", k), v.as_str().unwrap_or(""));
        }
    }
    out
}

fn describe_pod(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    let node = kvs(v, &["spec", "nodeName"]).unwrap_or_default();
    let phase = kvs(v, &["status", "phase"]).unwrap_or_default();
    let pod_ip = kvs(v, &["status", "podIP"]).unwrap_or_default();
    let host_ip = kvs(v, &["status", "hostIP"]).unwrap_or_default();
    push_kv(&mut out, "Node", node);
    push_kv(&mut out, "Phase", phase);
    push_kv(&mut out, "PodIP", pod_ip);
    push_kv(&mut out, "HostIP", host_ip);
    if let Some(conds) = kv(v, &["status", "conditions"]).and_then(|x| x.as_array()) {
        out.push_str("Conditions:\n");
        for c in conds {
            let t = c.get("type").and_then(|s| s.as_str()).unwrap_or("");
            let s = c.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let r = c.get("reason").and_then(|s| s.as_str()).unwrap_or("");
            push_kv(&mut out, &format!("  {}", t), format!("{} {}", s, r));
        }
    }
    if let Some(conts) = kv(v, &["spec", "containers"]).and_then(|x| x.as_array()) {
        out.push_str("Containers:\n");
        for c in conts {
            let n = c.get("name").and_then(|s| s.as_str()).unwrap_or("");
            let img = c.get("image").and_then(|s| s.as_str()).unwrap_or("");
            push_kv(&mut out, &format!("  {}", n), img);
        }
    }
    if let Some(sts) = kv(v, &["status", "containerStatuses"]).and_then(|x| x.as_array()) {
        out.push_str("Container Statuses:\n");
        for s in sts {
            let n = s.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let rc = s.get("restartCount").and_then(|x| x.as_i64()).unwrap_or(0);
            let ready = s.get("ready").and_then(|x| x.as_bool()).unwrap_or(false);
            let state = s.get("state").and_then(|x| x.as_object());
            let mut st = String::new();
            if let Some(sto) = state {
                if let Some(r) = sto.get("running").and_then(|x| x.as_object()) {
                    st = format!(
                        "running since {}",
                        r.get("startedAt").and_then(|x| x.as_str()).unwrap_or("")
                    );
                } else if let Some(w) = sto.get("waiting").and_then(|x| x.as_object()) {
                    st = format!(
                        "waiting: {}",
                        w.get("reason").and_then(|x| x.as_str()).unwrap_or("")
                    );
                } else if let Some(t) = sto.get("terminated").and_then(|x| x.as_object()) {
                    st = format!(
                        "terminated: {}",
                        t.get("reason").and_then(|x| x.as_str()).unwrap_or("")
                    );
                }
            }
            push_kv(
                &mut out,
                &format!("  {}", n),
                format!("ready={} restarts={} {}", ready, rc, st),
            );
        }
    }
    out
}

fn describe_deployment(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    let spec_rep = kv(v, &["spec", "replicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let ready = kv(v, &["status", "readyReplicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let updated = kv(v, &["status", "updatedReplicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let available = kv(v, &["status", "availableReplicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    push_kv(&mut out, "Replicas (spec)", spec_rep.to_string());
    push_kv(
        &mut out,
        "Replicas (ready/updated/available)",
        format!("{}/{}/{}", ready, updated, available),
    );
    if let Some(st) = kv(v, &["spec", "strategy", "type"]).and_then(|x| x.as_str()) {
        push_kv(&mut out, "Strategy", st);
    }
    if let Some(sel) = kv(v, &["spec", "selector", "matchLabels"]).and_then(|x| x.as_object()) {
        out.push_str("Selector:\n");
        for (k, v) in sel {
            push_kv(&mut out, &format!("  {}", k), v.as_str().unwrap_or(""));
        }
    }
    if let Some(tl) = kv(v, &["spec", "template", "metadata", "labels"]).and_then(|x| x.as_object())
    {
        out.push_str("Template Labels:\n");
        for (k, v) in tl {
            push_kv(&mut out, &format!("  {}", k), v.as_str().unwrap_or(""));
        }
    }
    if let Some(conts) =
        kv(v, &["spec", "template", "spec", "containers"]).and_then(|x| x.as_array())
    {
        out.push_str("Images:\n");
        for c in conts {
            let img = c.get("image").and_then(|s| s.as_str()).unwrap_or("");
            let n = c.get("name").and_then(|s| s.as_str()).unwrap_or("");
            push_kv(&mut out, &format!("  {}", n), img);
        }
    }
    if let Some(conds) = kv(v, &["status", "conditions"]).and_then(|x| x.as_array()) {
        out.push_str("Conditions:\n");
        for c in conds {
            let t = c.get("type").and_then(|s| s.as_str()).unwrap_or("");
            let s = c.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let r = c.get("reason").and_then(|s| s.as_str()).unwrap_or("");
            push_kv(&mut out, &format!("  {}", t), format!("{} {}", s, r));
        }
    }
    out
}

fn describe_service(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    let typ = kvs(v, &["spec", "type"]).unwrap_or_default();
    let cluster_ip = kvs(v, &["spec", "clusterIP"]).unwrap_or_default();
    push_kv(&mut out, "Type", typ);
    push_kv(&mut out, "ClusterIP", cluster_ip);
    if let Some(sel) = kv(v, &["spec", "selector"]).and_then(|x| x.as_object()) {
        out.push_str("Selector:\n");
        for (k, v) in sel {
            push_kv(&mut out, &format!("  {}", k), v.as_str().unwrap_or(""));
        }
    }
    if let Some(ports) = kv(v, &["spec", "ports"]).and_then(|x| x.as_array()) {
        out.push_str("Ports:\n");
        for p in ports {
            let port = p.get("port").and_then(|x| x.as_i64()).unwrap_or(0);
            let proto = p.get("protocol").and_then(|x| x.as_str()).unwrap_or("TCP");
            let target = p
                .get("targetPort")
                .map(|x| match x {
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::String(s) => s.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
            push_kv(
                &mut out,
                &format!("  {}", name),
                format!("{}/{} -> {}", port, proto, target),
            );
        }
    }
    out
}

fn describe_node(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    if let Some(addrs) = kv(v, &["status", "addresses"]).and_then(|x| x.as_array()) {
        out.push_str("Addresses:\n");
        for a in addrs {
            let t = a.get("type").and_then(|x| x.as_str()).unwrap_or("");
            let a = a.get("address").and_then(|x| x.as_str()).unwrap_or("");
            push_kv(&mut out, &format!("  {}", t), a);
        }
    }
    if let Some(cap) = kv(v, &["status", "capacity"]).and_then(|x| x.as_object()) {
        out.push_str("Capacity:\n");
        for (k, v) in cap {
            push_kv(
                &mut out,
                &format!("  {}", k),
                v.as_str().unwrap_or(&v.to_string()),
            );
        }
    }
    if let Some(conds) = kv(v, &["status", "conditions"]).and_then(|x| x.as_array()) {
        out.push_str("Conditions:\n");
        for c in conds {
            let t = c.get("type").and_then(|s| s.as_str()).unwrap_or("");
            let s = c.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let r = c.get("reason").and_then(|s| s.as_str()).unwrap_or("");
            push_kv(&mut out, &format!("  {}", t), format!("{} {}", s, r));
        }
    }
    out
}

async fn fetch_events_for(ns: &Option<String>, uid_str: &Option<String>, _gvk_key: &str) -> String {
    use kube::api::ListParams;
    let client = match orka_kubehub::get_kube_client().await {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    // Try events.k8s.io/v1 first, fallback to core/v1
    // Attempt field selector by UID if present; else by name will be ambiguous, so skip.
    let uid = match uid_str {
        Some(u) if !u.is_empty() => u.clone(),
        _ => return String::new(),
    };
    // events.k8s.io/v1
    if let Ok((ar, namespaced)) = orka_kubehub::get_api_resource("events.k8s.io/v1/Event").await {
        let api: Api<DynamicObject> = if namespaced {
            match ns {
                Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
                None => Api::all_with(client.clone(), &ar),
            }
        } else {
            Api::all_with(client.clone(), &ar)
        };
        let mut lp = ListParams::default();
        lp = lp.limit(200);
        lp = lp.fields(&format!("regarding.uid={}", uid));
        if let Ok(evlist) = api.list(&lp).await {
            let mut lines: Vec<String> = Vec::new();
            for e in evlist.items {
                if let Some(line) = event_line_from_dynamic(&e) {
                    lines.push(line);
                }
            }
            if !lines.is_empty() {
                return lines.join("\n");
            }
        }
    }
    // core/v1 fallback
    if let Ok((ar, namespaced)) = orka_kubehub::get_api_resource("v1/Event").await {
        let api: Api<DynamicObject> = if namespaced {
            match ns {
                Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
                None => Api::all_with(client.clone(), &ar),
            }
        } else {
            Api::all_with(client.clone(), &ar)
        };
        let mut lp = ListParams::default();
        lp = lp.limit(200);
        lp = lp.fields(&format!("involvedObject.uid={}", uid));
        if let Ok(evlist) = api.list(&lp).await {
            let mut lines: Vec<String> = Vec::new();
            for e in evlist.items {
                if let Some(line) = event_line_from_dynamic(&e) {
                    lines.push(line);
                }
            }
            if !lines.is_empty() {
                return lines.join("\n");
            }
        }
    }
    String::new()
}

fn event_line_from_dynamic(e: &DynamicObject) -> Option<String> {
    let v = serde_json::to_value(e).ok()?;
    // Try new API
    let t = v
        .get("eventTime")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("deprecatedLastTimestamp").and_then(|x| x.as_str()))
        .unwrap_or("");
    let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    let reason = v.get("reason").and_then(|x| x.as_str()).unwrap_or("");
    let note = v
        .get("note")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("message").and_then(|x| x.as_str()))
        .unwrap_or("");
    let count = v
        .get("deprecatedCount")
        .and_then(|x| x.as_i64())
        .or_else(|| v.get("count").and_then(|x| x.as_i64()))
        .unwrap_or(0);
    Some(format!(
        "{} {:>7} {:<24} {}{}",
        t,
        typ,
        reason,
        note,
        if count > 1 {
            format!(" (x{})", count)
        } else {
            String::new()
        }
    ))
}

fn describe_replicaset(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    let spec_rep = kv(v, &["spec", "replicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let ready = kv(v, &["status", "readyReplicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let available = kv(v, &["status", "availableReplicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let fully = kv(v, &["status", "fullyLabeledReplicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    push_kv(&mut out, "Replicas (spec)", spec_rep.to_string());
    push_kv(
        &mut out,
        "Replicas (ready/available/fully)",
        format!("{}/{}/{}", ready, available, fully),
    );
    out
}

fn describe_statefulset(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    let spec_rep = kv(v, &["spec", "replicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let ready = kv(v, &["status", "readyReplicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let current = kv(v, &["status", "currentReplicas"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    push_kv(&mut out, "Replicas (spec)", spec_rep.to_string());
    push_kv(
        &mut out,
        "Replicas (ready/current)",
        format!("{}/{}", ready, current),
    );
    if let Some(st) = kv(v, &["spec", "updateStrategy", "type"]).and_then(|x| x.as_str()) {
        push_kv(&mut out, "UpdateStrategy", st);
    }
    if let Some(svc) = kvs(v, &["spec", "serviceName"]) {
        push_kv(&mut out, "ServiceName", svc);
    }
    if let Some(vcts) = kv(v, &["spec", "volumeClaimTemplates"]).and_then(|x| x.as_array()) {
        if !vcts.is_empty() {
            out.push_str("VolumeClaims:\n");
            for t in vcts {
                let n = kvs(t, &["metadata", "name"]).unwrap_or_default();
                push_kv(&mut out, "  ", n);
            }
        }
    }
    out
}

fn describe_daemonset(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    let desired = kv(v, &["status", "desiredNumberScheduled"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let updated = kv(v, &["status", "updatedNumberScheduled"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let ready = kv(v, &["status", "numberReady"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let available = kv(v, &["status", "numberAvailable"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    push_kv(
        &mut out,
        "Desired/Updated/Ready/Available",
        format!("{}/{}/{}/{}", desired, updated, ready, available),
    );
    out
}

fn describe_job(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    let parallelism = kv(v, &["spec", "parallelism"])
        .and_then(|x| x.as_i64())
        .unwrap_or(1);
    let completions = kv(v, &["spec", "completions"])
        .and_then(|x| x.as_i64())
        .unwrap_or(1);
    push_kv(&mut out, "Parallelism", parallelism.to_string());
    push_kv(&mut out, "Completions", completions.to_string());
    let succeeded = kv(v, &["status", "succeeded"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let failed = kv(v, &["status", "failed"])
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    push_kv(
        &mut out,
        "Succeeded/Failed",
        format!("{}/{}", succeeded, failed),
    );
    if let Some(t) = kvs(v, &["status", "startTime"]) {
        push_kv(&mut out, "StartTime", t);
    }
    if let Some(t) = kvs(v, &["status", "completionTime"]) {
        push_kv(&mut out, "CompletionTime", t);
    }
    out
}

fn describe_cronjob(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    if let Some(s) = kvs(v, &["spec", "schedule"]) {
        push_kv(&mut out, "Schedule", s);
    }
    if let Some(sus) = kv(v, &["spec", "suspend"]).and_then(|x| x.as_bool()) {
        push_kv(&mut out, "Suspend", sus.to_string());
    }
    if let Some(n) = kv(v, &["spec", "successfulJobsHistoryLimit"]).and_then(|x| x.as_i64()) {
        push_kv(&mut out, "History(success)", n.to_string());
    }
    if let Some(n) = kv(v, &["spec", "failedJobsHistoryLimit"]).and_then(|x| x.as_i64()) {
        push_kv(&mut out, "History(failed)", n.to_string());
    }
    if let Some(t) = kvs(v, &["status", "lastScheduleTime"]) {
        push_kv(&mut out, "LastSchedule", t);
    }
    if let Some(act) = kv(v, &["status", "active"]).and_then(|x| x.as_array()) {
        push_kv(&mut out, "Active Jobs", act.len().to_string());
    }
    out
}

fn describe_ingress(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    if let Some(cn) = kvs(v, &["spec", "ingressClassName"]) {
        push_kv(&mut out, "Class", cn);
    }
    if let Some(rules) = kv(v, &["spec", "rules"]).and_then(|x| x.as_array()) {
        out.push_str("Rules:\n");
        for r in rules {
            let host = r.get("host").and_then(|x| x.as_str()).unwrap_or("");
            if let Some(http) = r
                .get("http")
                .and_then(|x| x.get("paths"))
                .and_then(|x| x.as_array())
            {
                for p in http {
                    let path = p.get("path").and_then(|x| x.as_str()).unwrap_or("/");
                    let svc = p
                        .get("backend")
                        .and_then(|b| b.get("service"))
                        .and_then(|s| s.get("name"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let port = p
                        .get("backend")
                        .and_then(|b| b.get("service"))
                        .and_then(|s| s.get("port"))
                        .and_then(|p| p.get("number"))
                        .and_then(|x| x.as_i64())
                        .unwrap_or(0);
                    push_kv(
                        &mut out,
                        &format!("  {}{}", host, path),
                        format!("{}:{}", svc, port),
                    );
                }
            }
        }
    }
    out
}

fn describe_configmap(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    if let Some(data) = kv(v, &["data"]).and_then(|x| x.as_object()) {
        out.push_str("Data keys:\n");
        for k in data.keys() {
            push_kv(&mut out, "  ", k);
        }
    }
    out
}

fn describe_secret(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    if let Some(t) = kvs(v, &["type"]) {
        push_kv(&mut out, "Type", t);
    }
    if let Some(data) = kv(v, &["data"]).and_then(|x| x.as_object()) {
        out.push_str("Data keys:\n");
        for k in data.keys() {
            push_kv(&mut out, "  ", k);
        }
    }
    out
}

fn describe_pvc(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    if let Some(phase) = kvs(v, &["status", "phase"]) {
        push_kv(&mut out, "Phase", phase);
    }
    if let Some(sc) = kvs(v, &["spec", "storageClassName"]) {
        push_kv(&mut out, "StorageClass", sc);
    }
    if let Some(am) = kv(v, &["spec", "accessModes"]).and_then(|x| x.as_array()) {
        let modes: Vec<String> = am
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect();
        push_kv(&mut out, "AccessModes", modes.join(", "));
    }
    if let Some(req) = kv(v, &["spec", "resources", "requests", "storage"]).and_then(|x| x.as_str())
    {
        push_kv(&mut out, "Requested", req);
    }
    if let Some(cap) = kv(v, &["status", "capacity", "storage"]).and_then(|x| x.as_str()) {
        push_kv(&mut out, "Capacity", cap);
    }
    out
}

fn describe_namespace(v: &serde_json::Value) -> String {
    let mut out = describe_generic(v);
    if let Some(phase) = kvs(v, &["status", "phase"]) {
        push_kv(&mut out, "Phase", phase);
    }
    out
}
