Configuration — Environment Variables

Core/runtime
- `ORKA_LOG` — logging level (`info`, `debug`)
- `ORKA_USE_API` — prefer API façade path (`1` default)
- `ORKA_METRICS_ADDR` — Prometheus exporter address (`host:port`)

Kube/discovery
- `ORKA_DISCOVERY_PATH` — disk cache directory for discovery
- `ORKA_DISCOVERY_TTL_SECS` — discovery cache TTL (default 86400)
- `ORKA_MEASURE_TRAFFIC` — measure snapshot/watch bytes (`1` to enable)

Listing/snapshot
- `ORKA_SNAPSHOT_PAGE_LIMIT` — server list pagination size (default 500)
- `ORKA_RELIST_SECS` — periodic relist interval (seconds)
- `ORKA_WATCH_BACKOFF_MAX_SECS` — max backoff between watch restarts (seconds)
- `ORKA_QUEUE_CAP` — internal channel capacity for deltas (default 2048)
- `ORKA_LIST_LITE_BUILTINS` — enable lite list path for built‑ins (`1` default)
- `ORKA_LIST_LITE_GROUPS` — comma list of groups allowed for lite list (`*` default)
- `ORKA_LITE_PROJECT` — project built‑in columns during lite processing (`1` default)

Schema (CRDs)
- `ORKA_DEFER_SCHEMA` — keep schema lookup out of snapshot critical path (`1` default)
- `ORKA_SCHEMA_OFFLINE_ONLY` — never fetch CRD schema from cluster (`0` default)
- `ORKA_SCHEMA_BUILTIN_SKIP` — skip schema for built‑ins (`1` default)

Memory/index pressure
- `ORKA_MAX_LABELS_PER_OBJ` — cap labels kept per object (default 128)
- `ORKA_MAX_ANNOS_PER_OBJ` — cap annotations kept per object (default 64)
- `ORKA_MAX_POSTINGS_PER_KEY` — cap per‑key postings in the index
- `ORKA_MAX_RSS_MB` — soft cap on in‑RAM snapshot size; trims annotations/labels/projected in stages
- `ORKA_MAX_INDEX_BYTES` — soft cap on index size (bytes)

Search
- `ORKA_SEARCH_LIMIT` — default search limit
- `ORKA_SEARCH_MAX_CANDIDATES` — cap candidates after typed filters
- `ORKA_SEARCH_MIN_SCORE` — minimum fuzzy score to include a hit

GUI
- `ORKA_RESULTS_SOFT_CAP` — soft row cap for results (default 2000)
- `ORKA_LOGS_BACKLOG_CAP` — legacy backlog ring capacity
- `ORKA_LOGS_RING_CAP` — new logs ring capacity (default 10000)
- `ORKA_LOGS_VISIBLE_FOLLOW_LIMIT` — draw limit while following (default 1000)
- `ORKA_LOGS_COLORIZE` — ANSI colorize logs (`1` default)
- `ORKA_LOGS_WRAP` — wrap long lines (`0` default)
- `ORKA_LOGS_ORDER_BY_TS_WHEN_PAUSED` — reorder by timestamp when paused (`1` default)
- `ORKA_LOGS_V2` — opt into v2 log engine (`1` default)
- `ORKA_LOGS_FOLLOW_PAD_ROWS` — extra bottom padding rows while following (default 1)
- `ORKA_LOGS_PREFIX_THEME` — one of `bright`, `basic`, `gray`, `none` (default `bright`)
- `ORKA_DETAILS_TTL_SECS` — TTL for details cache (default 60)
- `ORKA_DETAILS_TABS_CAP` — max open Details tabs (default 8)
- `ORKA_UI_DEBOUNCE_MS` — debounce UI updates (default 100)
- `ORKA_STATS_REFRESH_OPEN_MS` — stats refresh period when open (default 5000)
- `ORKA_STATS_REFRESH_CLOSED_MS` — stats refresh period when closed (default 30000)
- `ORKA_WARN_PCT` — warn threshold for capacity gauges (default 0.80)
- `ORKA_ERR_PCT` — error threshold for capacity gauges (default 0.95)
- `ORKA_IDLE_FAST_MS` — fast idle repaint cadence (default 8)
- `ORKA_IDLE_SLOW_MS` — slow idle repaint cadence (default 120)
- `ORKA_IDLE_FAST_WINDOW_MS` — window keeping fast cadence after activity (default 1000)
- `ORKA_ATLAS` — enable Atlas view (`1` default)
- `ORKA_PREWARM_KINDS` — comma list of GVK keys to prewarm watchers for

Ops
- `ORKA_OPS_QUEUE_CAP` — logs channel capacity (default 1024)
- `ORKA_EXEC_BACKLOG_CAP` — exec backlog ring capacity (default 4000)
- `ORKA_PF_BIND` — bind address for port‑forward listener (default `127.0.0.1`)
- `ORKA_DRAIN_TIMEOUT_SECS` — drain timeout (default 300)
- `ORKA_DRAIN_POLL_SECS` — drain poll interval (default 2)

Apply / Persist
- `ORKA_MAX_YAML_BYTES` — YAML payload max bytes (default 1MiB)
- `ORKA_MAX_YAML_NODES` — YAML JSON node budget (default 100k)
- `ORKA_DISABLE_APPLY_PREFLIGHT` — skip preflight live RV check
- `ORKA_DISABLE_LASTAPPLIED` — do not persist last‑applied snapshots
- `ORKA_DB_PATH` — path to append‑only last‑applied log (default `~/.orka/lastapplied.log`)
- `ORKA_ZSTD_LEVEL` — compression level when feature `zstd` is enabled (optional)

Notes
- All boolean‑like vars accept `1/0`, `true/false`, `yes/no` (case‑insensitive).
- Use `rg -n "ORKA_" -S` in the repo to find additional switches.

