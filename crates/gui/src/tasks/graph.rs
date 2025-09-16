#![forbid(unsafe_code)]

use crate::model::{GraphEdge, GraphModel, GraphNode, GraphNodeRole};
use crate::{OrkaGuiApp, UiUpdate};
use orka_core::Uid;
use tracing::info;

use orka_api::ResourceRef;

impl OrkaGuiApp {
    fn ensure_updates_channel_for_graph(&mut self) -> std::sync::mpsc::Sender<UiUpdate> {
        if let Some(tx) = &self.watch.updates_tx {
            return tx.clone();
        }
        let (tx, rx) = std::sync::mpsc::channel::<UiUpdate>();
        self.watch.updates_tx = Some(tx.clone());
        self.watch.updates_rx = Some(rx);
        tx
    }

    pub(crate) fn start_graph_task(&mut self, uid: Uid) {
        // cancel previous graph task if any
        if let Some(task) = self.graph.task.take() {
            task.abort();
        }
        if let Some(stop) = self.graph.stop.take() {
            let _ = stop.send(());
        }
        self.graph.running = true;
        self.graph.text.clear();
        self.graph.error = None;
        self.graph.uid = Some(uid);

        // Resolve GVK + ns/name
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
                self.graph.running = false;
                return;
            }
        };
        let name = match name_opt {
            Some(n) => n,
            None => {
                self.graph.running = false;
                return;
            }
        };
        let tx = self.ensure_updates_channel_for_graph();
        let api = self.api.clone();
        let reference = ResourceRef {
            cluster: None,
            gvk: gvk.clone(),
            namespace: ns_opt.clone(),
            name: name.clone(),
        };
        self.graph.task = Some(tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            match api.get_raw(reference).await {
                Ok(bytes) => {
                    match serde_json::from_slice::<serde_json::Value>(&bytes) {
                        Ok(v) => {
                            // Build both text and model; send both updates for flexible UI paths
                            let text =
                                match build_graph_text(&api, &v, &gvk, ns_opt.as_deref()).await {
                                    Ok(s) => s,
                                    Err(e) => format!("graph: error building: {}", e),
                                };
                            let model =
                                match build_graph_model(&api, &v, &gvk, ns_opt.as_deref()).await {
                                    Ok(m) => m,
                                    Err(_e) => GraphModel::default(),
                                };
                            let _ = tx.send(UiUpdate::GraphReady { uid, text });
                            let _ = tx.send(UiUpdate::GraphModelReady { uid, model });
                        }
                        Err(_) => {
                            let text = String::from_utf8_lossy(&bytes).into_owned();
                            let _ = tx.send(UiUpdate::GraphReady { uid, text });
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(UiUpdate::GraphError {
                        uid,
                        error: e.to_string(),
                    });
                }
            }
            info!(took_ms = %t0.elapsed().as_millis(), "graph: task ended");
        }));
    }
}

fn owner_from_value(v: &serde_json::Value) -> Option<(String, String, String, String)> {
    // Returns (group, version, kind, name) of the controller owner if any
    let owners = v
        .get("metadata")
        .and_then(|m| m.get("ownerReferences"))
        .and_then(|x| x.as_array())?;
    // Prefer controller=true
    let mut cand = None;
    for o in owners {
        let kind = o
            .get("kind")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let name = o
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let apiv = o.get("apiVersion").and_then(|x| x.as_str()).unwrap_or("");
        let (group, version) = if let Some((g, v)) = apiv.split_once('/') {
            (g.to_string(), v.to_string())
        } else {
            (String::new(), apiv.to_string())
        };
        let is_controller = o
            .get("controller")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        if is_controller {
            return Some((group, version, kind, name));
        }
        if cand.is_none() {
            cand = Some((group, version, kind, name));
        }
    }
    cand
}

async fn build_graph_text(
    api: &std::sync::Arc<dyn orka_api::OrkaApi>,
    v: &serde_json::Value,
    gvk: &orka_api::ResourceKind,
    ns: Option<&str>,
) -> Result<String, String> {
    use metrics::histogram;
    let t0 = std::time::Instant::now();
    let mut out = String::new();
    // Header
    let name = v
        .get("metadata")
        .and_then(|m| m.get("name"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let ns_str = v
        .get("metadata")
        .and_then(|m| m.get("namespace"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let header = if gvk.namespaced {
        format!("{} {}/{}", gvk.kind, ns_str, name)
    } else {
        format!("{} {}", gvk.kind, name)
    };
    let _ = std::fmt::write(&mut out, format_args!("{}\n", header));
    // Owner chain (walk up preferred controller owner)
    out.push_str("\nOwner Chain:\n");
    let mut steps = 0usize;
    let mut cur_v = v.clone();
    while let Some((group, version, kind, oname)) = owner_from_value(&cur_v) {
        steps += 1;
        let _ = std::fmt::write(
            &mut out,
            format_args!(
                "  -> {}/{} {}\n",
                if group.is_empty() {
                    version.clone()
                } else {
                    format!("{}/{}", group, version)
                },
                kind,
                oname
            ),
        );
        if steps >= 5 {
            break;
        }
        // Fetch parent raw to continue walking
        let parent_ref = ResourceRef {
            cluster: None,
            gvk: orka_api::ResourceKind {
                group: group.clone(),
                version: version.clone(),
                kind: kind.clone(),
                namespaced: true,
            },
            namespace: ns.map(|s| s.to_string()),
            name: oname.clone(),
        };
        match api.get_raw(parent_ref).await {
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(pv) => {
                    cur_v = pv;
                }
                Err(_) => break,
            },
            Err(_) => break,
        }
    }

    // Direct relationships
    out.push_str("\nDirect:\n");
    let kind = gvk.kind.as_str();
    match kind {
        "Pod" => {
            // ServiceAccount
            if let Some(sa) = v
                .get("spec")
                .and_then(|s| s.get("serviceAccountName"))
                .and_then(|x| x.as_str())
            {
                let _ = std::fmt::write(&mut out, format_args!("  ServiceAccount: {}\n", sa));
            }
            // ConfigMaps and Secrets from volumes
            let mut cms: Vec<String> = Vec::new();
            let mut secs: Vec<String> = Vec::new();
            if let Some(vols) = v
                .get("spec")
                .and_then(|s| s.get("volumes"))
                .and_then(|x| x.as_array())
            {
                for vol in vols {
                    if let Some(cm) = vol
                        .get("configMap")
                        .and_then(|m| m.get("name"))
                        .and_then(|x| x.as_str())
                    {
                        cms.push(cm.to_string());
                    }
                    if let Some(sec) = vol.get("secret").and_then(|m| {
                        m.get("secretName")
                            .or_else(|| m.get("name"))
                            .and_then(|x| x.as_str())
                    }) {
                        secs.push(sec.to_string());
                    }
                }
            }
            cms.sort();
            cms.dedup();
            secs.sort();
            secs.dedup();
            if !cms.is_empty() {
                let _ =
                    std::fmt::write(&mut out, format_args!("  ConfigMaps: {}\n", cms.join(", ")));
            }
            if !secs.is_empty() {
                let _ = std::fmt::write(&mut out, format_args!("  Secrets: {}\n", secs.join(", ")));
            }
        }
        "Service" => {
            // Resolve Pods by selector
            if let Some(sel) = v
                .get("spec")
                .and_then(|s| s.get("selector"))
                .and_then(|x| x.as_object())
            {
                let mut items: Vec<String> = Vec::new();
                for (k, v) in sel.iter() {
                    if let Some(sv) = v.as_str() {
                        items.push(format!("{}={}", k, sv));
                    }
                }
                items.sort();
                let selector = items.join(",");
                let ns_for = ns.map(|s| s.to_string());
                let count = match count_pods_for_selector(ns_for.as_deref(), &selector).await {
                    Ok(n) => n,
                    Err(_) => 0,
                };
                let _ = std::fmt::write(
                    &mut out,
                    format_args!(
                        "  Pods: {} (selector: {})\n",
                        count,
                        if selector.is_empty() {
                            "(none)".to_string()
                        } else {
                            selector
                        }
                    ),
                );
            }
        }
        _ => {}
    }

    let took = t0.elapsed().as_millis() as f64;
    histogram!("ui_graph_build_ms", took);
    Ok(out)
}

async fn count_pods_for_selector(ns: Option<&str>, selector: &str) -> Result<usize, String> {
    use kube::{api::Api, core::DynamicObject};
    let client = orka_kubehub::get_kube_client()
        .await
        .map_err(|e| e.to_string())?;
    let (ar, namespaced) = orka_kubehub::get_api_resource("v1/Pod")
        .await
        .map_err(|e| e.to_string())?;
    let api: Api<DynamicObject> = if namespaced {
        match ns {
            Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
            None => Api::all_with(client.clone(), &ar),
        }
    } else {
        Api::all_with(client.clone(), &ar)
    };
    let mut lp = kube::api::ListParams::default();
    if !selector.is_empty() {
        lp = lp.labels(selector);
    }
    lp = lp.limit(500);
    let list = api.list(&lp).await.map_err(|e| e.to_string())?;
    Ok(list.items.len())
}

async fn build_graph_model(
    api: &std::sync::Arc<dyn orka_api::OrkaApi>,
    v: &serde_json::Value,
    gvk: &orka_api::ResourceKind,
    ns: Option<&str>,
) -> Result<GraphModel, String> {
    let mut model = GraphModel::default();

    // Root node
    let name = v
        .get("metadata")
        .and_then(|m| m.get("name"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let ns_str = v
        .get("metadata")
        .and_then(|m| m.get("namespace"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let root_label = if gvk.namespaced {
        format!("{} {}/{}", gvk.kind, ns_str, name)
    } else {
        format!("{} {}", gvk.kind, name)
    };
    let root_id = format!("root:{}:{}:{}:{}", gvk.group, gvk.version, gvk.kind, name);
    model.nodes.push(GraphNode {
        id: root_id.clone(),
        label: root_label,
        kind: gvk.kind.clone(),
        role: GraphNodeRole::Root,
    });

    // Owner chain (walk up preferred controller owner)
    let mut depth = 0usize;
    let mut cur_v = v.clone();
    let mut child_id = root_id.clone();
    while let Some((group, version, kind, oname)) = owner_from_value(&cur_v) {
        depth += 1;
        let owner_id = format!("own:{}/{}/{}:{}", group, version, kind, oname);
        let owner_label = format!(
            "{}/{} {}",
            if group.is_empty() {
                version.clone()
            } else {
                format!("{}/{}", group, version)
            },
            kind,
            oname
        );
        model.nodes.push(GraphNode {
            id: owner_id.clone(),
            label: owner_label,
            kind: kind.clone(),
            role: GraphNodeRole::OwnerChain(depth),
        });
        model.edges.push(GraphEdge {
            from: owner_id.clone(),
            to: child_id.clone(),
            label: Some("owner".into()),
        });
        if depth >= 5 {
            break;
        }
        // Fetch parent raw to continue walking
        let parent_ref = ResourceRef {
            cluster: None,
            gvk: orka_api::ResourceKind {
                group: group.clone(),
                version: version.clone(),
                kind: kind.clone(),
                namespaced: true,
            },
            namespace: ns.map(|s| s.to_string()),
            name: oname.clone(),
        };
        match api.get_raw(parent_ref).await {
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(pv) => {
                    cur_v = pv;
                    child_id = owner_id;
                }
                Err(_) => break,
            },
            Err(_) => break,
        }
    }

    // Direct relationships
    match gvk.kind.as_str() {
        "Pod" => {
            if let Some(sa) = v
                .get("spec")
                .and_then(|s| s.get("serviceAccountName"))
                .and_then(|x| x.as_str())
            {
                let id = format!("sa:{}:{}:{}", ns.unwrap_or(""), "v1", sa);
                model.nodes.push(GraphNode {
                    id: id.clone(),
                    label: format!("ServiceAccount {}", sa),
                    kind: "ServiceAccount".into(),
                    role: GraphNodeRole::Related("ServiceAccount".into()),
                });
                model.edges.push(GraphEdge {
                    from: id,
                    to: root_id.clone(),
                    label: Some("uses".into()),
                });
            }
            // ConfigMaps and Secrets from volumes
            let mut cms: Vec<String> = Vec::new();
            let mut secs: Vec<String> = Vec::new();
            if let Some(vols) = v
                .get("spec")
                .and_then(|s| s.get("volumes"))
                .and_then(|x| x.as_array())
            {
                for vol in vols {
                    if let Some(cm) = vol
                        .get("configMap")
                        .and_then(|m| m.get("name"))
                        .and_then(|x| x.as_str())
                    {
                        cms.push(cm.to_string());
                    }
                    if let Some(sec) = vol.get("secret").and_then(|m| {
                        m.get("secretName")
                            .or_else(|| m.get("name"))
                            .and_then(|x| x.as_str())
                    }) {
                        secs.push(sec.to_string());
                    }
                }
            }
            cms.sort();
            cms.dedup();
            secs.sort();
            secs.dedup();
            for cm in cms {
                let id = format!("cm:{}", cm);
                model.nodes.push(GraphNode {
                    id: id.clone(),
                    label: format!("ConfigMap {}", cm),
                    kind: "ConfigMap".into(),
                    role: GraphNodeRole::Related("ConfigMap".into()),
                });
                model.edges.push(GraphEdge {
                    from: id,
                    to: root_id.clone(),
                    label: Some("mounts".into()),
                });
            }
            for sec in secs {
                let id = format!("sec:{}", sec);
                model.nodes.push(GraphNode {
                    id: id.clone(),
                    label: format!("Secret {}", sec),
                    kind: "Secret".into(),
                    role: GraphNodeRole::Related("Secret".into()),
                });
                model.edges.push(GraphEdge {
                    from: id,
                    to: root_id.clone(),
                    label: Some("mounts".into()),
                });
            }
        }
        "Service" => {
            if let Some(sel) = v
                .get("spec")
                .and_then(|s| s.get("selector"))
                .and_then(|x| x.as_object())
            {
                let mut items: Vec<String> = Vec::new();
                for (k, v) in sel.iter() {
                    if let Some(sv) = v.as_str() {
                        items.push(format!("{}={}", k, sv));
                    }
                }
                items.sort();
                let selector = items.join(",");
                let ns_for = ns.map(|s| s.to_string());
                let count = match count_pods_for_selector(ns_for.as_deref(), &selector).await {
                    Ok(n) => n,
                    Err(_) => 0,
                };
                let label = if selector.is_empty() {
                    "Pods".to_string()
                } else {
                    format!("Pods ({})", count)
                };
                let id = format!("pods:{}", selector);
                model.nodes.push(GraphNode {
                    id: id.clone(),
                    label,
                    kind: "Pods".into(),
                    role: GraphNodeRole::Related("Pods".into()),
                });
                model.edges.push(GraphEdge {
                    from: root_id.clone(),
                    to: id,
                    label: Some("selects".into()),
                });
            }
        }
        _ => {}
    }

    Ok(model)
}
