use std::str::FromStr;

use anyhow::Result;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use tracing::{error, info, warn};
use orka_store::spawn_ingest_with_projector;
use tokio::sync::mpsc;
use std::collections::HashMap;
use tokio::signal;
use std::time::{Duration, Instant};
use orka_persist::Store;

#[derive(Parser, Debug)]
#[command(name = "orkactl", version, about = "Orka CLI (M2)")]
struct Cli {
    /// Output format
    #[arg(short = 'o', long = "output", value_enum, global = true, default_value_t = Output::Human)]
    output: Output,

    /// Kubernetes namespace (default: current context)
    #[arg(long = "ns", global = true)]
    namespace: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum Output { Human, Json }

#[derive(Subcommand, Debug)]
enum Commands {
    /// Discover served resources (incl. CRDs)
    Discover {
        /// Prefer selecting a CRD (for demos)
        #[arg(long = "prefer-crd", action = ArgAction::SetTrue)]
        prefer_crd: bool,
    },
    /// List objects for a given group/version/kind key
    Ls {
        /// GVK key, e.g. "v1/ConfigMap" or "cert-manager.io/v1/Certificate"
        gvk: String,
    },
    /// Watch objects for a GVK and print +/- events
    Watch {
        /// GVK key, e.g. "v1/ConfigMap" or "cert-manager.io/v1/Certificate"
        gvk: String,
    },
    /// Inspect schema details for a GVK (CRDs only)
    Schema {
        /// GVK key, e.g. "cert-manager.io/v1/Certificate"
        gvk: String,
    },
    /// Search current snapshot (simple RAM index)
    Search {
        /// GVK key to watch while indexing
        gvk: String,
        /// Query string (supports free text + typed filters)
        query: String,
        /// Limit results
        #[arg(long = "limit", default_value_t = 20, env = "ORKA_SEARCH_LIMIT")]
        limit: usize,
        /// Maximum candidates after typed filters
        #[arg(long = "max-candidates", env = "ORKA_SEARCH_MAX_CANDIDATES")]
        max_candidates: Option<usize>,
        /// Minimum fuzzy score to include hit
        #[arg(long = "min-score", env = "ORKA_SEARCH_MIN_SCORE")]
        min_score: Option<f32>,
        /// Explain filter stages and counts
        #[arg(long = "explain", action = ArgAction::SetTrue)]
        explain: bool,
    },
    /// Edit a resource from a YAML file (dry-run or apply)
    Edit {
        /// YAML path or '-' for stdin
        #[arg(short = 'f', long = "file")]
        file: String,
        /// Validate against CRD JSONSchema (feature-gated)
        #[arg(long = "validate", action = ArgAction::SetTrue)]
        validate: bool,
        /// Perform a server-side dry-run
        #[arg(long = "dry-run", action = ArgAction::SetTrue)]
        dry_run: bool,
        /// Apply with SSA (fieldManager=orka)
        #[arg(long = "apply", action = ArgAction::SetTrue)]
        apply: bool,
    },
    /// Show minimal diffs vs live and last-applied
    Diff {
        /// YAML path or '-' for stdin
        #[arg(short = 'f', long = "file")]
        file: String,
    },
    /// Inspect last-applied snapshots for a resource
    #[command(name = "last-applied")]
    LastApplied {
        #[command(subcommand)]
        sub: LastAppliedCmd,
    },
}

#[derive(Subcommand, Debug)]
enum LastAppliedCmd {
    /// Get last-applied snapshots for a resource
    Get {
        /// GVK key, e.g. "v1/ConfigMap" or "group/v1/Foo"
        gvk: String,
        /// Resource name
        name: String,
        /// Limit number of entries
        #[arg(long = "limit", default_value_t = 3)]
        limit: usize,
        /// Output YAML payloads as JSON array
        #[arg(short = 'o', long = "output", value_enum)]
        output: Option<Output>,
    },
}

fn init_tracing() {
    let env = std::env::var("ORKA_LOG").unwrap_or_else(|_| "info".to_string());
    let filter = tracing_subscriber::EnvFilter::from_str(&env).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(true).init();
}

fn init_metrics() {
    if let Ok(addr) = std::env::var("ORKA_METRICS_ADDR") {
        if let Ok(sock) = addr.parse::<std::net::SocketAddr>() {
            let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
            match builder.with_http_listener(sock).install() {
                Ok(_) => tracing::info!(addr = %addr, "Prometheus metrics exporter listening"),
                Err(e) => tracing::warn!(error = %e, "failed to install metrics exporter"),
            }
        } else {
            tracing::warn!(addr = %addr, "invalid ORKA_METRICS_ADDR; expected host:port");
        }
    }
}

fn parse_gvk(key: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = key.split('/').collect();
    match parts.as_slice() {
        [version, kind] => Some((String::new(), (*version).to_string(), (*kind).to_string())),
        [group, version, kind] => Some(((*group).to_string(), (*version).to_string(), (*kind).to_string())),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    init_metrics();
    let cli = Cli::parse();

    match cli.command {
        Commands::Discover { prefer_crd } => {
            info!(prefer_crd, "discover invoked");
            match orka_kubehub::discover(prefer_crd).await {
                Ok(resources) => match cli.output {
                    Output::Human => {
                        for r in resources {
                            let scope = if r.namespaced { "namespaced" } else { "cluster" };
                            let gv = if r.group.is_empty() { r.version.clone() } else { format!("{}/{}", r.group, r.version) };
                            println!("{} • {} • {}", gv, r.kind, scope);
                        }
                    }
                    Output::Json => println!("{}", serde_json::to_string_pretty(&resources)?),
                },
                Err(e) => {
                    error!(error = ?e, "discover failed");
                    eprintln!("discover error: {}", e);
                }
            }
        }
        Commands::Ls { gvk } => {
            let ns = cli.namespace.as_deref();
            info!(gvk = %gvk, ns = ?ns, "ls invoked");
            let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
            let projector = match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => Some(std::sync::Arc::new(schema.projector()) as std::sync::Arc<dyn orka_core::Projector + Send + Sync>),
                _ => None,
            };
            let (ingest_tx, backend) = spawn_ingest_with_projector(cap, projector);
            // Start watcher
            let watcher_handle = tokio::spawn({
                let gvk = gvk.clone();
                let ns = ns.map(|s| s.to_string());
                let tx = ingest_tx.clone();
                async move {
                    if let Err(e) = orka_kubehub::start_watcher(&gvk, ns.as_deref(), tx).await {
                        error!(error = ?e, "watcher failed");
                    }
                }
            });
            // Prime initial list for faster first snapshot
            let _ = orka_kubehub::prime_list(&gvk, ns, &ingest_tx).await;

            // Wait for first epoch (configurable)
            let wait_secs = std::env::var("ORKA_WAIT_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(8);
            let mut rx = backend.subscribe_epoch();
            let deadline = Instant::now() + Duration::from_secs(wait_secs);
            while *rx.borrow() == 0 {
                let now = Instant::now();
                if now >= deadline { break; }
                let rem = deadline.duration_since(now).min(Duration::from_secs(2));
                if tokio::time::timeout(rem, rx.changed()).await.is_err() { break; }
            }
            let snap = backend.current();

            match cli.output {
                Output::Human => {
                    println!("NAMESPACE   NAME                 AGE");
                    for item in snap.items.iter().filter(|o| ns.map(|n| o.namespace.as_deref() == Some(n)).unwrap_or(true)) {
                        let ns_col = item.namespace.clone().unwrap_or_else(|| "-".to_string());
                        let age = render_age(item.creation_ts);
                        println!("{:<11} {:<20} {}", ns_col, item.name, age);
                    }
                }
                Output::Json => {
                    // Filter by namespace if provided
                    let items: Vec<_> = snap.items
                        .iter()
                        .filter(|o| ns.map(|n| o.namespace.as_deref() == Some(n)).unwrap_or(true))
                        .cloned()
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&items)?);
                }
            }
            // Graceful shutdown: drop last sender and abort watcher to close ingest and flush
            drop(ingest_tx);
            watcher_handle.abort();
        }
        Commands::Watch { gvk } => {
            let ns = cli.namespace.as_deref();
            info!(gvk = %gvk, ns = ?ns, "watch invoked");
            let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
            let projector = match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => Some(std::sync::Arc::new(schema.projector()) as std::sync::Arc<dyn orka_core::Projector + Send + Sync>),
                _ => None,
            };
            let (ingest_tx, _backend) = spawn_ingest_with_projector(cap, projector);
            let (tap_tx, mut tap_rx) = mpsc::channel::<orka_core::Delta>(cap);

            // Start watcher writing into our tap
            let watcher_handle = tokio::spawn({
                let gvk = gvk.clone();
                let ns = ns.map(|s| s.to_string());
                let tap_tx = tap_tx.clone();
                async move {
                    if let Err(e) = orka_kubehub::start_watcher(&gvk, ns.as_deref(), tap_tx).await {
                        error!(error = ?e, "watcher failed");
                    }
                }
            });

            // Pump deltas into ingest and print lines directly
            let mut seen_rv: HashMap<orka_core::Uid, String> = HashMap::new();
            loop {
                tokio::select! {
                    maybe = tap_rx.recv() => {
                        match maybe {
                            Some(d) => {
                                // forward to ingest (best-effort)
                                let _ = ingest_tx.send(d.clone()).await;

                                // filter by ns if provided
                                if let Some(ns_filter) = ns {
                                    if let Some(mns) = d.raw.get("metadata").and_then(|m| m.get("namespace")).and_then(|v| v.as_str()) {
                                        if mns != ns_filter { continue; }
                                    } else {
                                        // cluster-scoped won't match a namespaced filter
                                        continue;
                                    }
                                }
                                let key = json_key(&d.raw);
                                let rv = d
                                    .raw
                                    .get("metadata")
                                    .and_then(|m| m.get("resourceVersion"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                match d.kind {
                                    orka_core::DeltaKind::Applied => {
                                        match seen_rv.get_mut(&d.uid) {
                                            None => {
                                                seen_rv.insert(d.uid, rv);
                                                println!("+ {}", key);
                                            }
                                            Some(prev_rv) => {
                                                if *prev_rv != rv {
                                                    *prev_rv = rv;
                                                    println!("+ {}", key);
                                                } // else duplicate; ignore
                                            }
                                        }
                                    }
                                    orka_core::DeltaKind::Deleted => {
                                        let _ = seen_rv.remove(&d.uid);
                                        println!("- {}", key);
                                    }
                                }
                            }
                            None => {
                                warn!("tap channel closed; exiting watch loop");
                                break;
                            }
                        }
                    }
                    _ = signal::ctrl_c() => {
                        info!("Ctrl-C received; shutting down watch loop");
                        break;
                    }
                }
            }

            // Graceful shutdown: close ingest and abort watcher to stop kube stream, allowing final snapshot flush
            drop(ingest_tx);
            watcher_handle.abort();
            warn!("watch loop ended (graceful shutdown)");
        }
        Commands::Schema { gvk } => {
            info!(gvk = %gvk, "schema invoked");
            match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => match cli.output {
                    Output::Human => {
                        println!("served: {}", schema.served_version);
                        if schema.printer_cols.is_empty() {
                            println!("printer-cols: (none)");
                        } else {
                            let cols: Vec<_> = schema.printer_cols.iter().map(|c| c.name.as_str()).collect();
                            println!("printer-cols: {}", cols.join(", "));
                        }
                        if schema.projected_paths.is_empty() {
                            println!("projected: (heuristic defaults)");
                        } else {
                            let proj: Vec<_> = schema.projected_paths.iter().map(|p| p.json_path.as_str()).collect();
                            println!("projected: {}", proj.join(", "));
                        }
                    }
                    Output::Json => {
                        println!("{}", serde_json::to_string_pretty(&schema)?);
                    }
                },
                Ok(None) => {
                    eprintln!("no CRD schema for builtin kind (or not found)");
                }
                Err(e) => {
                    eprintln!("schema error: {}", e);
                }
            }
        }
        Commands::Search { gvk, query, limit, max_candidates, min_score, explain } => {
            // Choose watcher namespace: CLI --ns overrides, else extract from query ns:token
            let ns_from_query = query.split_whitespace().find_map(|t| t.strip_prefix("ns:")).map(|s| s.to_string());
            let effective_ns = cli.namespace.clone().or(ns_from_query);
            let ns = effective_ns.as_deref();
            info!(gvk = %gvk, ns = ?ns, query = %query, limit, "search invoked");
            let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
            let projector = match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => Some(std::sync::Arc::new(schema.projector()) as std::sync::Arc<dyn orka_core::Projector + Send + Sync>),
                _ => None,
            };
            let (ingest_tx, backend) = spawn_ingest_with_projector(cap, projector);
            // Start watcher
            let watcher_handle = tokio::spawn({
                let gvk = gvk.clone();
                let ns = ns.map(|s| s.to_string());
                let tx = ingest_tx.clone();
                async move {
                    if let Err(e) = orka_kubehub::start_watcher(&gvk, ns.as_deref(), tx).await {
                        error!(error = ?e, "watcher failed");
                    }
                }
            });
            // Prime initial list so snapshot has data before waiting
            let _ = orka_kubehub::prime_list(&gvk, ns, &ingest_tx).await;

            // Wait for first epoch (configurable)
            let wait_secs = std::env::var("ORKA_WAIT_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(8);
            let mut rx = backend.subscribe_epoch();
            let deadline = Instant::now() + Duration::from_secs(wait_secs);
            while *rx.borrow() == 0 {
                let now = Instant::now();
                if now >= deadline { break; }
                let rem = deadline.duration_since(now).min(Duration::from_secs(2));
                if tokio::time::timeout(rem, rx.changed()).await.is_err() { break; }
            }
            let snap = backend.current();
            // Build index with field mapping (if schema known)
            let field_pairs: Option<Vec<(String, u32)>> = match orka_schema::fetch_crd_schema(&gvk).await {
                Ok(Some(schema)) => Some(schema.projected_paths.iter().map(|p| (p.json_path.clone(), p.id)).collect()),
                _ => None,
            };
            let (group_str, _version_str, kind_str) = parse_gvk(&gvk).unwrap_or((String::new(), String::new(), String::new()));
            let index = match field_pairs {
                Some(pairs) => orka_search::Index::build_from_snapshot_with_meta(&snap, Some(&pairs), Some(&kind_str), Some(&group_str)),
                None => orka_search::Index::build_from_snapshot_with_meta(&snap, None, Some(&kind_str), Some(&group_str)),
            };
            let opts = orka_search::SearchOpts { max_candidates, min_score };
            let (hits, dbg) = index.search_with_debug_opts(&query, limit, opts);

            match cli.output {
                Output::Human => {
                    println!("KIND   NAMESPACE/NAME                SCORE");
                    for h in hits {
                        if let Some(obj) = snap.items.get(h.doc as usize) {
                            let ns_col = obj.namespace.clone().unwrap_or_else(|| "-".to_string());
                            println!("{:<6} {:<22} {:<20} {:.2}", kind_str, format!("{}/{}", ns_col, obj.name), "", h.score);
                        }
                    }
                }
                Output::Json => {
                    #[derive(serde::Serialize)]
                    struct Row<'a> { ns: &'a str, name: &'a str, score: f32 }
                    let rows: Vec<_> = hits
                        .into_iter()
                        .filter_map(|h| {
                            snap.items.get(h.doc as usize).map(|o| Row { ns: o.namespace.as_deref().unwrap_or("") , name: &o.name, score: h.score })
                        })
                        .collect();
                    if explain {
                        #[derive(serde::Serialize)]
                        struct Explain<'a, T> { hits: T, debug: &'a orka_search::SearchDebugInfo }
                        println!("{}", serde_json::to_string_pretty(&Explain { hits: rows, debug: &dbg })?);
                    } else {
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                    }
                }
            }
            if explain && matches!(cli.output, Output::Human) {
                eprintln!("debug: total={} after_ns={} after_label_keys={} after_labels={} after_anno_keys={} after_annos={} after_fields={}", dbg.total, dbg.after_ns, dbg.after_label_keys, dbg.after_labels, dbg.after_anno_keys, dbg.after_annos, dbg.after_fields);
            }

            // Shutdown
            drop(ingest_tx);
            watcher_handle.abort();
        }
        Commands::Edit { file, validate, dry_run, apply } => {
            let ns = cli.namespace.as_deref();
            let yaml = read_input(&file)?;
            if validate {
                #[cfg(feature = "validate")]
                {
                    // Detect GVK from YAML for schema lookup
                    let j: serde_yaml::Value = serde_yaml::from_str(&yaml)?;
                    let api_ver = j.get("apiVersion").and_then(|v| v.as_str()).unwrap_or("");
                    let kind = j.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    let gvk_key = if api_ver.contains('/') { format!("{}/{}", api_ver, kind) } else { format!("{}/{}", api_ver, kind) };
                    let issues = orka_schema::validate::validate_yaml_for_gvk(&gvk_key, &yaml).await?;
                    if !issues.is_empty() {
                        eprintln!("validation issues ({}):", issues.len());
                        for it in issues { eprintln!("- {}: {}{}", it.path, it.error, it.hint.as_deref().map(|h| format!(" ({})", h)).unwrap_or_default()); }
                    }
                }
                #[cfg(not(feature = "validate"))]
                {
                    warn!("validate flag set but CLI built without 'validate' feature");
                }
            }
            let do_apply = if apply { true } else { !dry_run };
            match orka_apply::edit_from_yaml(&yaml, ns, validate, do_apply).await {
                Ok(res) => match cli.output {
                    Output::Human => {
                        if res.dry_run {
                            println!("dry-run: +{} ~{} -{}", res.summary.adds, res.summary.updates, res.summary.removes);
                        } else if res.applied {
                            println!("applied rv={}", res.new_rv.unwrap_or_default());
                        } else {
                            println!("no-op");
                        }
                    }
                    Output::Json => println!("{}", serde_json::to_string_pretty(&res)?),
                },
                Err(e) => { eprintln!("edit error: {}", e); }
            }
        }
        Commands::Diff { file } => {
            let ns = cli.namespace.as_deref();
            let yaml = read_input(&file)?;
            match orka_apply::diff_from_yaml(&yaml, ns).await {
                Ok((live, last)) => match cli.output {
                    Output::Human => {
                        println!("vs live: +{} ~{} -{}", live.adds, live.updates, live.removes);
                        if let Some(ls) = last { println!("vs last: +{} ~{} -{}", ls.adds, ls.updates, ls.removes); }
                    }
                    Output::Json => {
                        #[derive(serde::Serialize)]
                        struct D { live: orka_apply::DiffSummary, last: Option<orka_apply::DiffSummary> }
                        println!("{}", serde_json::to_string_pretty(&D { live, last })?);
                    }
                },
                Err(e) => eprintln!("diff error: {}", e),
            }
        }
        Commands::LastApplied { sub } => {
            match sub {
                LastAppliedCmd::Get { gvk, name, limit, output } => {
                    // Resolve UID by fetching live object
                    let ns = cli.namespace.as_deref();
                    let uid_hex = fetch_uid_for(&gvk, &name, ns).await?;
                    let uid = parse_uid(&uid_hex)?;
                    let store = match orka_persist::SqliteStore::open_default() { Ok(s) => s, Err(e) => { eprintln!("open db error: {}", e); return Ok(()); } };
                    let rows = store.get_last(uid, Some(limit)).unwrap_or_default();
                    match output.unwrap_or(cli.output) {
                        Output::Human => {
                            for r in rows.iter() {
                                let ts = r.ts;
                                println!("ts={} rv={}", ts, r.rv);
                            }
                        }
                        Output::Json => {
                            #[derive(serde::Serialize)]
                            struct Row { ts: i64, rv: String, yaml: String }
                            let out: Vec<Row> = rows.into_iter().map(|r| Row { ts: r.ts, rv: r.rv, yaml: orka_persist::maybe_decompress(&r.yaml_zstd) }).collect();
                            println!("{}", serde_json::to_string_pretty(&out)?);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn render_age(creation_ts: i64) -> String {
    if creation_ts <= 0 { return "-".to_string(); }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let mut secs = (now - creation_ts).max(0) as u64;
    let days = secs / 86_400; secs %= 86_400;
    let hours = secs / 3600; secs %= 3600;
    let mins = secs / 60; secs %= 60;
    if days > 0 { format!("{}d{}h", days, hours) }
    else if hours > 0 { format!("{}h{}m", hours, mins) }
    else if mins > 0 { format!("{}m", mins) }
    else { format!("{}s", secs) }
}

fn json_key(v: &serde_json::Value) -> String {
    let meta = v.get("metadata");
    let name = meta.and_then(|m| m.get("name")).and_then(|v| v.as_str()).unwrap_or("");
    if let Some(ns) = meta.and_then(|m| m.get("namespace")).and_then(|v| v.as_str()) {
        format!("{}/{}", ns, name)
    } else {
        name.to_string()
    }
}

fn read_input(path: &str) -> Result<String> {
    if path == "-" {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s)
    } else {
        Ok(std::fs::read_to_string(path)?)
    }
}

async fn fetch_uid_for(gvk_key: &str, name: &str, namespace: Option<&str>) -> Result<String> {
    use kube::{discovery::{Discovery, Scope}, api::Api, core::{DynamicObject, GroupVersionKind}};
    let client = kube::Client::try_default().await?;
    // Parse key
    let (group, version, kind) = parse_gvk(gvk_key).ok_or_else(|| anyhow::anyhow!("invalid gvk: {}", gvk_key))?;
    let gvk = GroupVersionKind { group, version, kind };
    // Find ApiResource
    let discovery = Discovery::new(client.clone()).run().await?;
    let mut ar_opt: Option<(kube::core::ApiResource, bool)> = None;
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            if ar.group == gvk.group && ar.version == gvk.version && ar.kind == gvk.kind {
                ar_opt = Some((ar.clone(), matches!(caps.scope, Scope::Namespaced)));
                break;
            }
        }
    }
    let (ar, namespaced) = ar_opt.ok_or_else(|| anyhow::anyhow!("GVK not found: {}/{}/{}", gvk.group, gvk.version, gvk.kind))?;
    let api: Api<DynamicObject> = if namespaced {
        match namespace {
            Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
            None => return Err(anyhow::anyhow!("namespace required for namespaced kind")),
        }
    } else { Api::all_with(client.clone(), &ar) };
    let obj = api.get(name).await?;
    let uid = obj.metadata.uid.ok_or_else(|| anyhow::anyhow!("object missing metadata.uid"))?;
    Ok(uid)
}

fn parse_uid(uid_str: &str) -> Result<orka_core::Uid> {
    let u = uuid::Uuid::parse_str(uid_str).map_err(|e| anyhow::anyhow!("invalid uid: {}", e))?;
    Ok(*u.as_bytes())
}
