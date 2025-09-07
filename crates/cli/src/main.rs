use std::str::FromStr;

use anyhow::Result;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use tracing::{error, info, warn};
use orka_store::spawn_ingest;
use tokio::sync::mpsc;
use std::collections::HashMap;

#[derive(Parser, Debug)]
#[command(name = "orkactl", version, about = "Orka CLI (M0)")]
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
}

fn init_tracing() {
    let env = std::env::var("ORKA_LOG").unwrap_or_else(|_| "info".to_string());
    let filter = tracing_subscriber::EnvFilter::from_str(&env).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(true).init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
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
            let (tx, backend) = spawn_ingest(cap);
            // Start watcher
            tokio::spawn({
                let gvk = gvk.clone();
                let ns = ns.map(|s| s.to_string());
                async move {
                    if let Err(e) = orka_kubehub::start_watcher(&gvk, ns.as_deref(), tx).await {
                        error!(error = ?e, "watcher failed");
                    }
                }
            });

            // Wait briefly for first snapshot; fallback to empty
            let mut rx = backend.subscribe_epoch();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx.changed()).await;
            let snap = backend.current();

            // Render table
            println!("NAMESPACE   NAME                 AGE");
            for item in snap.items.iter().filter(|o| ns.map(|n| o.namespace.as_deref() == Some(n)).unwrap_or(true)) {
                let ns_col = item.namespace.clone().unwrap_or_else(|| "-".to_string());
                let age = render_age(item.creation_ts);
                println!("{:<11} {:<20} {}", ns_col, item.name, age);
            }
        }
        Commands::Watch { gvk } => {
            let ns = cli.namespace.as_deref();
            info!(gvk = %gvk, ns = ?ns, "watch invoked");
            let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
            let (ingest_tx, _backend) = spawn_ingest(cap);
            let (tap_tx, mut tap_rx) = mpsc::channel::<orka_core::Delta>(cap);

            // Start watcher writing into our tap
            tokio::spawn({
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
            while let Some(d) = tap_rx.recv().await {
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
                                    println!("~ {}", key);
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
            warn!("watch loop ended (watcher closed)");
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
