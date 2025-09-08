//! Orka Ops (Milestone OPS 3.1): imperative Kubernetes operations.
//! Initial scaffold with `logs` implemented and other ops stubbed.

#![forbid(unsafe_code)]

use anyhow::{anyhow, Result};
use futures::StreamExt;
use kube::{api::{Api, LogParams, Patch, PatchParams, PostParams, ListParams, DeleteParams}, Client};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// A single chunk of log output (line oriented for now).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogChunk {
    pub line: String,
}

/// Options for `logs` operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogOptions {
    /// Follow the stream (default: true)
    pub follow: bool,
    /// Tail last n lines (server-side), if supported
    pub tail_lines: Option<i64>,
    /// Only return logs newer than X seconds
    pub since_seconds: Option<i64>,
}

/// Cancellation handle for an in-flight operation.
#[derive(Debug)]
pub struct CancelHandle {
    tx: Option<oneshot::Sender<()>>,
}

impl CancelHandle {
    pub fn cancel(mut self) {
        if let Some(tx) = self.tx.take() { let _ = tx.send(()); }
    }
}

/// Result of starting a streaming operation.
pub struct StreamHandle<T> {
    pub rx: mpsc::Receiver<T>,
    pub cancel: CancelHandle,
}

/// Imperative ops trait. Methods may stream or perform one-shot mutations.
#[allow(unused_variables)]
#[async_trait::async_trait]
pub trait OrkaOps: Send + Sync {
    /// Stream logs from a pod/container.
    async fn logs(&self, namespace: Option<&str>, pod: &str, container: Option<&str>, opts: LogOptions) -> Result<StreamHandle<LogChunk>>;

    // Stubs for upcoming ops in this milestone
    async fn exec(&self, namespace: Option<&str>, pod: &str, container: Option<&str>, cmd: &[String], pty: bool) -> Result<()> { Err(anyhow!("exec: not implemented yet")) }
    async fn port_forward(&self, namespace: Option<&str>, pod: &str, local: u16, remote: u16) -> Result<StreamHandle<ForwardEvent>> { Err(anyhow!("port-forward: not implemented yet")) }
    async fn scale(&self, gvk_key: &str, namespace: Option<&str>, name: &str, replicas: i32, use_subresource: bool) -> Result<()>;
    async fn rollout_restart(&self, gvk_key: &str, namespace: Option<&str>, name: &str) -> Result<()>;
    async fn delete_pod(&self, namespace: &str, pod: &str, grace_seconds: Option<i64>) -> Result<()>;
    async fn cordon(&self, node: &str, on: bool) -> Result<()>;
    async fn drain(&self, node: &str) -> Result<()>;

    /// Discover capabilities (RBAC + subresources) for current user/context.
    async fn caps(&self, namespace: Option<&str>, scale_gvk: Option<&str>) -> Result<OpsCaps>;
}

/// Default implementation using kube-rs client APIs.
pub struct KubeOps;

impl KubeOps {
    pub fn new() -> Self { Self }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpsCaps {
    pub namespace: Option<String>,
    pub pods_log_get: bool,
    pub pods_exec_create: bool,
    pub pods_portforward_create: bool,
    pub nodes_patch: bool,
    pub pods_eviction_create: Option<bool>,
    pub scale: Option<ScaleCaps>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaleCaps {
    pub gvk: String,
    pub resource: String,
    pub subresource_patch: bool,
    pub spec_replicas_patch: bool,
}

impl KubeOps {
    async fn ssar_check(client: Client, ns: Option<&str>, group: &str, resource: &str, subresource: Option<&str>, verb: &str) -> Result<bool> {
        use k8s_openapi::api::authorization::v1::{ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec};
        let api: Api<SelfSubjectAccessReview> = Api::all(client);
        let ra = ResourceAttributes {
            group: if group.is_empty() { None } else { Some(group.to_string()) },
            resource: Some(resource.to_string()),
            subresource: subresource.map(|s| s.to_string()),
            verb: Some(verb.to_string()),
            namespace: ns.map(|s| s.to_string()),
            ..Default::default()
        };
        let ssar = SelfSubjectAccessReview {
            spec: SelfSubjectAccessReviewSpec { resource_attributes: Some(ra), ..Default::default() },
            ..Default::default()
        };
        let created = api.create(&PostParams::default(), &ssar).await?;
        Ok(created.status.map(|s| s.allowed).unwrap_or(false))
    }

    /// Discover imperative ops capabilities (subresources + RBAC) for current user.
    /// If `scale_gvk` is provided (e.g. "apps/v1/Deployment"), also probes Scale subresource.
    pub async fn discover_caps(namespace: Option<&str>, scale_gvk: Option<&str>) -> Result<OpsCaps> {
        let client = Client::try_default().await?;
        let ns_owned = namespace.map(|s| s.to_string());
        // Pods subresources (namespace-scoped)
        let pods_log_get = KubeOps::ssar_check(client.clone(), namespace, "", "pods", Some("log"), "get").await.unwrap_or(false);
        let pods_exec_create = KubeOps::ssar_check(client.clone(), namespace, "", "pods", Some("exec"), "create").await.unwrap_or(false);
        let pods_portforward_create = KubeOps::ssar_check(client.clone(), namespace, "", "pods", Some("portforward"), "create").await.unwrap_or(false);
        // Nodes patch (cluster-scoped)
        let nodes_patch = KubeOps::ssar_check(client.clone(), None, "", "nodes", None, "patch").await.unwrap_or(false);
        // Eviction create (namespace-scoped, group=policy)
        let pods_eviction_create = match namespace { Some(ns) => Some(KubeOps::ssar_check(client.clone(), Some(ns), "policy", "pods", Some("eviction"), "create").await.unwrap_or(false)), None => None };

        // Scale subresource for provided GVK
        let mut scale_caps: Option<ScaleCaps> = None;
        if let Some(gvk_key) = scale_gvk {
            use kube::core::GroupVersionKind;
            let (group, version, kind) = parse_gvk_key(gvk_key)?;
            let gvk = GroupVersionKind { group, version, kind };
            let (ar, namespaced) = find_api_resource(client.clone(), &gvk).await?;
            let resource = ar.plural.clone();
            // Check patch on subresource 'scale'
            let sub_patch = KubeOps::ssar_check(client.clone(), if namespaced { namespace } else { None }, ar.group.as_str(), resource.as_str(), Some("scale"), "patch").await.unwrap_or(false);
            // Check patch on main resource (SSA fallback of .spec.replicas)
            let spec_patch = KubeOps::ssar_check(client.clone(), if namespaced { namespace } else { None }, ar.group.as_str(), resource.as_str(), None, "patch").await.unwrap_or(false);
            scale_caps = Some(ScaleCaps { gvk: format!("{}/{}/{}", ar.group, ar.version, ar.kind), resource, subresource_patch: sub_patch, spec_replicas_patch: spec_patch });
        }

        Ok(OpsCaps {
            namespace: ns_owned,
            pods_log_get,
            pods_exec_create,
            pods_portforward_create,
            nodes_patch,
            pods_eviction_create,
            scale: scale_caps,
        })
    }

    /// Instance wrapper for capability discovery.
    pub async fn caps(&self, namespace: Option<&str>, scale_gvk: Option<&str>) -> Result<OpsCaps> {
        // Reuse the static helper to avoid duplicating logic
        Self::discover_caps(namespace, scale_gvk).await
    }
}

#[async_trait::async_trait]
impl OrkaOps for KubeOps {
    async fn logs(&self, namespace: Option<&str>, pod: &str, container: Option<&str>, opts: LogOptions) -> Result<StreamHandle<LogChunk>> {
        use k8s_openapi::api::core::v1::Pod;

        let client = Client::try_default().await?;
        let api: Api<Pod> = match namespace {
            Some(ns) => Api::namespaced(client, ns),
            None => return Err(anyhow!("namespace is required for pod logs")),
        };

        let mut lp = LogParams::default();
        lp.follow = opts.follow;
        lp.tail_lines = opts.tail_lines.map(|v| v as i64);
        lp.since_seconds = opts.since_seconds.map(|v| v as i64);
        if let Some(c) = container { lp.container = Some(c.to_string()); }

        let cap = std::env::var("ORKA_OPS_QUEUE_CAP").ok().and_then(|s| s.parse().ok()).unwrap_or(1024);
        let (tx, rx) = mpsc::channel::<LogChunk>(cap);
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let cancel = CancelHandle { tx: Some(cancel_tx) };

        // Spawn a task to stream logs and forward into bounded channel.
        let pod_name = pod.to_string();
        let container_label = container.map(|s| s.to_string());
        tokio::spawn(async move {
            use tokio_util::{compat::FuturesAsyncReadCompatExt, io::ReaderStream};
            info!(pod = %pod_name, container = ?container_label, follow = lp.follow, tail = ?lp.tail_lines, since = ?lp.since_seconds, "logs stream starting");
            let stream_res = api.log_stream(&pod_name, &lp).await;
            let reader = match stream_res {
                Ok(r) => r,
                Err(e) => { warn!(error = %e, "log_stream failed to open"); return; }
            };
            // Convert futures::io::AsyncRead into tokio::io::AsyncRead, then into a bytes Stream
            let compat_reader = reader.compat();
            let stream = ReaderStream::new(compat_reader);
            pump_bytes_to_lines(stream, tx, cancel_rx, Some(&pod_name)).await;
        });

        Ok(StreamHandle { rx, cancel })
    }

    async fn exec(&self, namespace: Option<&str>, pod: &str, container: Option<&str>, cmd: &[String], pty: bool) -> Result<()> {
        use k8s_openapi::api::core::v1::Pod;
        use kube::api::{AttachParams, TerminalSize};
        use futures::SinkExt;
        use tokio::io::AsyncWriteExt;
        use std::io::Read;

        struct RawGuard;
        impl Drop for RawGuard { fn drop(&mut self) { let _ = crossterm::terminal::disable_raw_mode(); } }

        let ns = namespace.ok_or_else(|| anyhow!("namespace is required for exec"))?;
        let client = Client::try_default().await?;
        let api: Api<Pod> = Api::namespaced(client, ns);
        let mut ap = if pty { AttachParams::interactive_tty() } else { AttachParams::default() };
        if let Some(c) = container { ap = ap.container(c); }
        if pty { ap = ap.stderr(false); } else { ap = ap.stdout(true).stderr(true); }

        let mut attached = api.exec(pod, cmd.to_vec(), &ap).await?;

        // TTY raw mode + resize support
        let mut resize_task = None;
        let _raw_guard = if pty {
            let _ = crossterm::terminal::enable_raw_mode();
            // Initial size and SIGWINCH updates
            if let Some(mut tx) = attached.terminal_size() {
                let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
                let _ = tx.send(TerminalSize { height: h as u16, width: w as u16 }).await;
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sig = signal(SignalKind::window_change())?;
                    resize_task = Some(tokio::spawn(async move {
                        while sig.recv().await.is_some() {
                            if let Ok((w, h)) = crossterm::terminal::size() {
                                let _ = tx.send(TerminalSize { height: h as u16, width: w as u16 }).await;
                            }
                        }
                    }));
                }
            }
            Some(RawGuard)
        } else { None };

        // stdin: spawn blocking reader thread -> channel -> async writer
        let mut stdin_task = None;
        if pty {
            // Only wire stdin in TTY/interactive mode
            let mut writer = attached.stdin().expect("stdin writer missing");
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);
            std::thread::spawn(move || {
                let mut input = std::io::stdin();
                let mut buf = [0u8; 1024];
                loop {
                    match input.read(&mut buf) {
                        Ok(0) => { let _ = tx.blocking_send(Vec::new()); break; }
                        Ok(n) => { let _ = tx.blocking_send(buf[..n].to_vec()); }
                        Err(_) => { let _ = tx.blocking_send(Vec::new()); break; }
                    }
                }
            });
            stdin_task = Some(tokio::spawn(async move {
                while let Some(chunk) = rx.recv().await { if chunk.is_empty() { break; } if writer.write_all(&chunk).await.is_err() { break; } }
            }));
        }

        // stdout/stderr pumps
        let mut out_task = None;
        if let Some(stdout_reader) = attached.stdout() {
            let mut stream = tokio_util::io::ReaderStream::new(stdout_reader);
            let task = tokio::spawn(async move {
                while let Some(Ok(bytes)) = stream.next().await { print!("{}", String::from_utf8_lossy(&bytes)); }
            });
            out_task = Some(task);
        }
        let mut err_task = None;
        if let Some(stderr_reader) = attached.stderr() {
            let mut stream = tokio_util::io::ReaderStream::new(stderr_reader);
            let task = tokio::spawn(async move {
                while let Some(Ok(bytes)) = stream.next().await { eprint!("{}", String::from_utf8_lossy(&bytes)); }
            });
            err_task = Some(task);
        }

        // Wait for remote to terminate OR Ctrl-C for graceful abort
        tokio::select! {
            _ = attached.join() => {
                // remote process finished
            }
            _ = tokio::signal::ctrl_c() => {
                warn!("Ctrl-C received during exec; closing session");
                // Drop attached to close underlying streams/websocket
            }
        }
        // Don't await stdin task; process is over. It will end when process exits.
        if let Some(t) = stdin_task { t.abort(); }
        if let Some(t) = out_task { let _ = t.await; }
        if let Some(t) = err_task { let _ = t.await; }
        if let Some(t) = resize_task { let _ = t.await; }
        drop(_raw_guard); // ensure raw mode disabled
        Ok(())
    }

    async fn port_forward(&self, namespace: Option<&str>, pod: &str, local: u16, remote: u16) -> Result<StreamHandle<ForwardEvent>> {
        let ns = namespace.ok_or_else(|| anyhow!("namespace is required for port-forward"))?;
        KubeOps::pf_internal(ns, pod, local, remote).await
    }

    async fn scale(&self, gvk_key: &str, namespace: Option<&str>, name: &str, replicas: i32, use_subresource: bool) -> Result<()> {
        use kube::core::{DynamicObject, GroupVersionKind};
        let client = Client::try_default().await?;
        let (group, version, kind) = parse_gvk_key(gvk_key)?;
        let gvk = GroupVersionKind { group, version, kind };
        let (ar, namespaced) = find_api_resource(client.clone(), &gvk).await?;
        let api_do: Api<DynamicObject> = if namespaced {
            match namespace { Some(ns) => Api::namespaced_with(client.clone(), ns, &ar), None => return Err(anyhow!("namespace required for namespaced kind")), }
        } else { Api::all_with(client.clone(), &ar) };

        // Try scale subresource if requested
        if use_subresource {
            let pp = PatchParams::default();
            let payload = serde_json::json!({"spec": {"replicas": replicas}});
            match api_do.patch_scale(name, &pp, &Patch::Merge(&payload)).await {
                Ok(_) => return Ok(()),
                Err(e) => warn!(error = %e, "patch_scale failed; falling back to spec.replicas"),
            }
        }
        // Fallback to patching .spec.replicas
        let pp = PatchParams::default();
        let payload = serde_json::json!({"spec": {"replicas": replicas}});
        let _ = api_do.patch(name, &pp, &Patch::Merge(&payload)).await?;
        Ok(())
    }

    async fn rollout_restart(&self, gvk_key: &str, namespace: Option<&str>, name: &str) -> Result<()> {
        use kube::{core::{DynamicObject, GroupVersionKind}};
        let client = Client::try_default().await?;
        let (group, version, kind) = parse_gvk_key(gvk_key)?;
        let gvk = GroupVersionKind { group, version, kind };
        let (ar, namespaced) = find_api_resource(client.clone(), &gvk).await?;
        let api_do: Api<DynamicObject> = if namespaced {
            match namespace { Some(ns) => Api::namespaced_with(client.clone(), ns, &ar), None => return Err(anyhow!("namespace required for namespaced kind")), }
        } else { Api::all_with(client.clone(), &ar) };
        let ts = chrono::Utc::now().to_rfc3339();
        let patch = serde_json::json!({
            "spec": {"template": {"metadata": {"annotations": {"kubectl.kubernetes.io/restartedAt": ts}}}}
        });
        let pp = PatchParams::default();
        let _ = api_do.patch(name, &pp, &Patch::Merge(&patch)).await?;
        Ok(())
    }

    async fn delete_pod(&self, namespace: &str, pod: &str, grace_seconds: Option<i64>) -> Result<()> {
        use k8s_openapi::api::core::v1::Pod;
        let client = Client::try_default().await?;
        let api: Api<Pod> = Api::namespaced(client, namespace);
        let dp = DeleteParams { grace_period_seconds: grace_seconds.map(|v| v as u32), ..Default::default() };
        let _ = api.delete(pod, &dp).await?;
        Ok(())
    }

    async fn cordon(&self, node: &str, on: bool) -> Result<()> {
        use k8s_openapi::api::core::v1::Node;
        let client = Client::try_default().await?;
        let api: Api<Node> = Api::all(client);
        let pp = PatchParams::default();
        let patch = serde_json::json!({"spec": {"unschedulable": on}});
        let _ = api.patch(node, &pp, &Patch::Merge(&patch)).await?;
        Ok(())
    }

    async fn drain(&self, node: &str) -> Result<()> {
        use k8s_openapi::api::core::v1::Pod;
        use std::collections::HashSet;
        let client = Client::try_default().await?;
        let all_pods: Api<Pod> = Api::all(client.clone());
        let lp = ListParams::default().fields(&format!("spec.nodeName={}", node));

        let timeout_secs: u64 = std::env::var("ORKA_DRAIN_TIMEOUT_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
        let poll_secs: u64 = std::env::var("ORKA_DRAIN_POLL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        // Helper to filter target pods for eviction
        let list_target = || async {
            let mut targets: Vec<(String,String)> = Vec::new();
            let pods = all_pods.list(&lp).await?;
            for p in pods.items {
                let meta = &p.metadata;
                let ns = match meta.namespace.clone() { Some(ns) => ns, None => continue };
                let name = match meta.name.clone() { Some(n) => n, None => continue };
                // Skip DaemonSet-managed pods (ownerReferences contains DaemonSet)
                let skip_ds = meta.owner_references.as_ref().map(|ors| ors.iter().any(|o| o.kind == "DaemonSet")).unwrap_or(false);
                // Skip mirror/static pods
                let is_mirror = meta.annotations.as_ref().and_then(|a| a.get("kubernetes.io/config.mirror")).is_some();
                if skip_ds || is_mirror { continue; }
                targets.push((ns, name));
            }
            Ok::<Vec<(String,String)>, anyhow::Error>(targets)
        };

        // Initial eviction attempts
        let mut pending: HashSet<(String,String)> = list_target().await?.into_iter().collect();
        let ep = kube::api::EvictParams::default();
        for (ns, name) in pending.clone().into_iter() {
            let pods_ns: Api<Pod> = Api::namespaced(client.clone(), &ns);
            match pods_ns.evict(&name, &ep).await {
                Ok(_) => {}
                Err(kube::Error::Api(ae)) if ae.code == 429 => {
                    // Blocked by PDB; keep in pending
                    warn!(ns = %ns, pod = %name, "eviction blocked by PDB (429) - will retry");
                }
                Err(e) => {
                    warn!(ns = %ns, pod = %name, error = %e, "eviction error - will retry");
                }
            }
        }

        // Wait loop until all targets are gone or timeout
        loop {
            // Refresh pending set from live cluster
            let fresh = list_target().await?;
            pending = fresh.into_iter().collect();
            if pending.is_empty() { break; }
            if std::time::Instant::now() >= deadline {
                let remain: Vec<String> = pending.iter().map(|(ns,n)| format!("{}/{}", ns, n)).collect();
                return Err(anyhow!("drain timeout; remaining: {}", remain.join(", ")));
            }
            // Re-attempt evictions for remaining pods
            for (ns, name) in pending.clone().into_iter() {
                let pods_ns: Api<Pod> = Api::namespaced(client.clone(), &ns);
                match pods_ns.evict(&name, &ep).await {
                    Ok(_) => {}
                    Err(kube::Error::Api(ae)) if ae.code == 429 => {
                        // PDB still preventing disruption; keep waiting
                    }
                    Err(_e) => { /* best-effort; try again next round */ }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
        }
        Ok(())
    }

    async fn caps(&self, namespace: Option<&str>, scale_gvk: Option<&str>) -> Result<OpsCaps> {
        KubeOps::discover_caps(namespace, scale_gvk).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ForwardEvent {
    Ready(String),
    Connected(String),
    Closed,
    Error(String),
}

impl KubeOps {
    async fn pf_internal(namespace: &str, pod: &str, local: u16, remote: u16) -> Result<StreamHandle<ForwardEvent>> {
        use k8s_openapi::api::core::v1::Pod;
        use tokio::net::TcpListener;
        let client = Client::try_default().await?;
        let api: Api<Pod> = Api::namespaced(client, namespace);
        let mut pf = api.portforward(pod, &[remote]).await?;
        let (tx, rx) = mpsc::channel::<ForwardEvent>(16);
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let cancel = CancelHandle { tx: Some(cancel_tx) };
        let bind_addr = std::env::var("ORKA_PF_BIND").unwrap_or_else(|_| "127.0.0.1".to_string());
        let listener = TcpListener::bind((bind_addr.as_str(), local)).await?;
        let actual = listener.local_addr()?;
        let _ = tx.send(ForwardEvent::Ready(actual.to_string())).await;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => { let _ = tx.send(ForwardEvent::Closed).await; break; }
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((mut inbound, peer)) => {
                                let _ = tx.send(ForwardEvent::Connected(peer.to_string())).await;
                                match pf.take_stream(remote) {
                                    Some(mut stream) => {
                                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut stream).await;
                                        // drop stream and connection; continue accepting
                                    }
                                    None => {
                                        let _ = tx.send(ForwardEvent::Error("pf stream missing".into())).await;
                                    }
                                }
                            }
                            Err(e) => { let _ = tx.send(ForwardEvent::Error(format!("accept error: {}", e))).await; break; }
                        }
                    }
                }
            }
        });
        Ok(StreamHandle { rx, cancel })
    }
}

/// Internal: consume a stream of bytes, split into lines, send via bounded channel.
/// Drops lines when channel is full. Flushes last partial line on end.
async fn pump_bytes_to_lines<S, E>(stream: S, tx: mpsc::Sender<LogChunk>, mut cancel_rx: oneshot::Receiver<()>, ctx: Option<&str>)
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>>,
    E: std::fmt::Display,
{
    let stream = stream.fuse();
    futures::pin_mut!(stream);
    let mut buf = bytes::BytesMut::new();
    loop {
        tokio::select! {
            _ = &mut cancel_rx => { if let Some(c) = ctx { info!(ctx = %c, "log pump cancelled"); } break; }
            next = stream.next() => {
                match next {
                    Some(Ok(chunk)) => {
                        buf.extend_from_slice(&chunk);
                        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                            let line = buf.split_to(pos);
                            let _ = buf.split_to(1); // drop '\n'
                            if let Ok(s) = std::str::from_utf8(&line) {
                                let _ = tx.try_send(LogChunk { line: s.to_string() });
                            }
                        }
                    }
                    Some(Err(e)) => { if let Some(c) = ctx { warn!(ctx = %c, error = %e, "log stream error"); } else { warn!(error = %e, "log stream error"); } break; }
                    None => break,
                }
            }
        }
    }
    if !buf.is_empty() {
        if let Ok(s) = std::str::from_utf8(&buf) {
            let _ = tx.try_send(LogChunk { line: s.to_string() });
        }
    }
    if let Some(c) = ctx { info!(ctx = %c, "log pump ended"); } else { info!("log pump ended"); }
}

fn parse_gvk_key(key: &str) -> Result<(String, String, String)> {
    let parts: Vec<_> = key.split('/').collect();
    match parts.as_slice() {
        [version, kind] => Ok((String::new(), (*version).to_string(), (*kind).to_string())),
        [group, version, kind] => Ok(((*group).to_string(), (*version).to_string(), (*kind).to_string())),
        _ => Err(anyhow!("invalid gvk key: {} (expect v1/Kind or group/v1/Kind)", key)),
    }
}

async fn find_api_resource(client: Client, gvk: &kube::core::GroupVersionKind) -> Result<(kube::core::ApiResource, bool)> {
    use kube::discovery::{Discovery, Scope};
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[tokio::test]
    async fn splits_lines_across_chunks_and_flushes_tail() {
        let (tx, mut rx) = mpsc::channel::<LogChunk>(16);
        let (_cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let chunks = vec![
            Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from_static(b"hello\nwor")),
            Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from_static(b"ld\n")),
            Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from_static(b"tail")),
        ];
        let s = stream::iter(chunks);
        pump_bytes_to_lines(s, tx, cancel_rx, Some("test")).await;
        let mut out = Vec::new();
        while let Some(c) = rx.recv().await { out.push(c.line); }
        assert_eq!(out, vec!["hello", "world", "tail"]);
    }

    #[tokio::test]
    async fn bounded_channel_drops_when_full() {
        let (tx, mut rx) = mpsc::channel::<LogChunk>(1);
        let (_cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let lines = vec![
            Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from_static(b"a\n")),
            Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from_static(b"b\n")),
            Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from_static(b"c\n")),
        ];
        let s = stream::iter(lines);
        pump_bytes_to_lines(s, tx, cancel_rx, Some("drop-test")).await;
        // We expect at least 1 line (the first), subsequent may be dropped due to full channel
        let mut recv = Vec::new();
        while let Ok(Some(c)) = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await { recv.push(c.line); }
        assert!(!recv.is_empty());
        assert!(recv.len() <= 2, "expected dropping when full (got {} lines)", recv.len());
    }

    #[tokio::test]
    async fn cancel_stops_pump_quickly() {
        let (tx, mut rx) = mpsc::channel::<LogChunk>(16);
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        // Slow stream: yields one big chunk after delay, then loops
        let s = async_stream::stream! {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                yield Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from_static(b"line\n"));
            }
        };
        let handle = tokio::spawn(pump_bytes_to_lines(s, tx, cancel_rx, Some("cancel-test")));
        // Cancel shortly after
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let _ = cancel_tx.send(());
        // Join with timeout to ensure it exits
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await.expect("pump did not stop");
        // Drain anything left
        let _ = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
    }

    #[test]
    fn parse_gvk_key_parses_core() {
        let (g, v, k) = parse_gvk_key("v1/ConfigMap").expect("ok");
        assert_eq!(g, "");
        assert_eq!(v, "v1");
        assert_eq!(k, "ConfigMap");
    }

    #[test]
    fn parse_gvk_key_parses_group() {
        let (g, v, k) = parse_gvk_key("apps/v1/Deployment").expect("ok");
        assert_eq!(g, "apps");
        assert_eq!(v, "v1");
        assert_eq!(k, "Deployment");
    }

    #[test]
    fn parse_gvk_key_invalid_returns_err() {
        assert!(parse_gvk_key("invalid").is_err());
        assert!(parse_gvk_key("").is_err());
        assert!(parse_gvk_key("a/b/c/d").is_err());
    }
}
