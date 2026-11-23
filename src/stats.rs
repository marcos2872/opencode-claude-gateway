//! Local usage accounting for `ocg --status`.
//!
//! Every 2xx `/v1/messages` forward records the upstream-reported token counts
//! per `(gateway id, opencode ref)` pair, bucketed by UTC day. `--status`
//! renders the last [`WINDOW_DAYS`] days as a share-of-total table with the
//! alias → destination conversion, so the launcher does not need a dashboard.
//!
//! Persistence is a single JSON file (`ocg.stats.json`, 0600) flushed on a
//! timer from the server and read by the CLI. It is best-effort: a missing or
//! corrupt file starts empty instead of breaking boot, and the in-memory view
//! is authoritative while the daemon is up.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Rolling window `--status` reports (today plus the six previous days).
pub const WINDOW_DAYS: u64 = 7;
/// Days kept on disk once pruned (the window plus a small margin).
const KEEP_DAYS: u64 = 9;
const SECONDS_PER_DAY: u64 = 86_400;
const BAR_WIDTH: usize = 15;

/// Counters for one `(gateway id, ref)` pair. `input` already includes
/// `cached` when the upstream reports prefix-cache hits; the share uses
/// `input + output` because that is what the provider bills.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cached: u64,
}

impl Totals {
    pub fn tokens(&self) -> u64 {
        self.input.saturating_add(self.output)
    }

    fn add(&mut self, other: &Totals) {
        self.requests = self.requests.saturating_add(other.requests);
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cached = self.cached.saturating_add(other.cached);
    }
}

/// Pairing of what the client asked for and what the gateway forwarded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Key {
    gateway: String,
    #[serde(rename = "ref")]
    model_ref: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Store {
    #[serde(default = "store_version")]
    version: u32,
    #[serde(default)]
    days: Vec<DayBucket>,
}

fn store_version() -> u32 {
    1
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: store_version(),
            days: vec![],
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DayBucket {
    day: u64,
    #[serde(default)]
    models: Vec<ModelRow>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModelRow {
    gateway: String,
    #[serde(rename = "ref")]
    model_ref: String,
    #[serde(flatten)]
    totals: Totals,
}

struct Inner {
    days: BTreeMap<u64, HashMap<Key, Totals>>,
    path: Option<PathBuf>,
    dirty: bool,
}

/// Shared, cheap to clone: the server writes, the CLI reads.
#[derive(Clone)]
pub struct UsageStats {
    inner: Arc<Mutex<Inner>>,
}

impl Default for UsageStats {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl UsageStats {
    /// Stats that never touch disk (tests, `--refresh`).
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                days: BTreeMap::new(),
                path: None,
                dirty: false,
            })),
        }
    }

    /// Load `path` if it exists; a corrupt file is reported and ignored.
    pub fn open(path: PathBuf) -> Self {
        let mut days: BTreeMap<u64, HashMap<Key, Totals>> = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<Store>(&text) {
                Ok(store) => {
                    for bucket in store.days {
                        let row = days.entry(bucket.day).or_default();
                        for m in bucket.models {
                            let key = Key {
                                gateway: m.gateway,
                                model_ref: m.model_ref,
                            };
                            row.entry(key).or_default().add(&m.totals);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "ignoring corrupt stats file")
                }
            }
        }
        let stats = Self {
            inner: Arc::new(Mutex::new(Inner {
                days,
                path: Some(path),
                dirty: false,
            })),
        };
        stats.prune(today());
        stats
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock must not disable accounting forever.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record one forwarded request. `input` / `output` / `cached` are the
    /// upstream-reported counters (zeros when it reported nothing).
    pub fn record(
        &self,
        gateway_model: &str,
        opencode_ref: &str,
        input: u64,
        output: u64,
        cached: u64,
    ) {
        self.record_on(
            today(),
            gateway_model,
            opencode_ref,
            &Totals {
                requests: 1,
                input,
                output,
                cached,
            },
        );
    }

    /// `record` with an explicit day (tests drive the window directly).
    pub fn record_on(&self, day: u64, gateway_model: &str, opencode_ref: &str, totals: &Totals) {
        let mut inner = self.lock();
        let key = Key {
            gateway: gateway_model.to_string(),
            model_ref: opencode_ref.to_string(),
        };
        inner
            .days
            .entry(day)
            .or_default()
            .entry(key)
            .or_default()
            .add(totals);
        inner.dirty = true;
    }

    /// Drop buckets older than [`KEEP_DAYS`] so the file cannot grow forever.
    fn prune(&self, now_day: u64) {
        let cutoff = now_day.saturating_sub(KEEP_DAYS);
        let mut inner = self.lock();
        let before = inner.days.len();
        inner.days.retain(|day, _| *day >= cutoff);
        if inner.days.len() != before {
            inner.dirty = true;
        }
    }

    /// Aggregated view of the last `window_days` days (ending today), sorted
    /// by tokens, then requests, then gateway id.
    pub fn snapshot(&self, window_days: u64) -> Snapshot {
        self.snapshot_ending(today(), window_days)
    }

    /// `snapshot` with an explicit end day (tests).
    pub fn snapshot_ending(&self, end_day: u64, window_days: u64) -> Snapshot {
        let start = end_day.saturating_sub(window_days.saturating_sub(1));
        let inner = self.lock();
        let mut merged: HashMap<Key, Totals> = HashMap::new();
        for (day, bucket) in inner
            .days
            .iter()
            .filter(|(d, _)| **d >= start && **d <= end_day)
        {
            let _ = day;
            for (key, totals) in bucket {
                merged.entry(key.clone()).or_default().add(totals);
            }
        }
        let mut rows: Vec<Row> = merged
            .into_iter()
            .map(|(key, totals)| Row {
                gateway_model: key.gateway,
                opencode_ref: key.model_ref,
                totals,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.totals
                .tokens()
                .cmp(&a.totals.tokens())
                .then_with(|| b.totals.requests.cmp(&a.totals.requests))
                .then_with(|| a.gateway_model.cmp(&b.gateway_model))
                .then_with(|| a.opencode_ref.cmp(&b.opencode_ref))
        });
        let mut out = Snapshot::default();
        for row in rows {
            out.requests = out.requests.saturating_add(row.totals.requests);
            out.input = out.input.saturating_add(row.totals.input);
            out.output = out.output.saturating_add(row.totals.output);
            out.cached = out.cached.saturating_add(row.totals.cached);
            out.rows.push(row);
        }
        out
    }

    /// Write the JSON snapshot back when something changed. Best-effort:
    /// the caller logs the error, the daemon keeps running in memory.
    pub fn flush(&self) -> std::io::Result<()> {
        let (path, body) = {
            let mut inner = self.lock();
            let Some(path) = inner.path.clone() else {
                return Ok(());
            };
            if !inner.dirty {
                return Ok(());
            }
            let store = Store {
                version: store_version(),
                days: inner
                    .days
                    .iter()
                    .map(|(day, rows)| DayBucket {
                        day: *day,
                        models: rows
                            .iter()
                            .map(|(key, totals)| ModelRow {
                                gateway: key.gateway.clone(),
                                model_ref: key.model_ref.clone(),
                                totals: *totals,
                            })
                            .collect(),
                    })
                    .collect(),
            };
            let body = serde_json::to_vec_pretty(&store)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            inner.dirty = false;
            (path, body)
        };
        write_private(&path, &body)
    }

    /// Whether there is anything pending to write (tests / status).
    pub fn is_dirty(&self) -> bool {
        self.lock().dirty
    }
}

/// `$OCG_STATS_FILE`, else `~/.local/share/opencode-claude-gateway/ocg.stats.json`.
pub fn default_path() -> PathBuf {
    if let Ok(p) = std::env::var("OCG_STATS_FILE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    crate::config::AppConfig::data_dir().join("ocg.stats.json")
}

/// Days since the Unix epoch in UTC (no date crate needed).
pub fn today() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / SECONDS_PER_DAY)
        .unwrap_or(0)
}

/// Aggregated counters over the whole window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub rows: Vec<Row>,
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cached: u64,
}

impl Snapshot {
    pub fn tokens(&self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub gateway_model: String,
    pub opencode_ref: String,
    pub totals: Totals,
}

/// The `--status` block. Pure: everything it needs is in `snapshot`.
pub fn render_report(snapshot: &Snapshot, window_days: u64) -> String {
    if snapshot.requests == 0 {
        return "no usage recorded yet (run `ocg claude` or send a request)".to_string();
    }
    let total = snapshot.tokens();
    let window = if window_days == 1 {
        "last 24h".to_string()
    } else {
        format!("last {window_days} days")
    };
    let mut out = String::new();
    if total == 0 {
        out.push_str(&format!(
            "Most used models · {window} · no token usage reported ({} requests)",
            snapshot.requests
        ));
    } else {
        out.push_str(&format!(
            "Most used models · {window} · {} tokens",
            fmt_tokens(total)
        ));
    }
    if snapshot.cached > 0 {
        out.push_str(&format!(" · {} cached", fmt_tokens(snapshot.cached)));
    }
    out.push('\n');

    let labels: Vec<String> = snapshot.rows.iter().map(label_of).collect();
    let width = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let max_tokens = snapshot
        .rows
        .iter()
        .map(|r| fmt_tokens(r.totals.tokens()).len())
        .max()
        .unwrap_or(1);
    let max_reqs = snapshot
        .rows
        .iter()
        .map(|r| r.totals.requests.to_string().len())
        .max()
        .unwrap_or(1);

    for (row, label) in snapshot.rows.iter().zip(labels.iter()) {
        out.push_str(&format!(
            "  {label:<width$}  {pct:>6} {bar} {tokens:>tokens_w$}  {reqs:>reqs_w$} req\n",
            width = width,
            pct = percent(row.totals.tokens(), total),
            bar = bar(row.totals.tokens(), total),
            tokens = fmt_tokens(row.totals.tokens()),
            tokens_w = max_tokens,
            reqs = row.totals.requests,
            reqs_w = max_reqs,
        ));
    }
    out.push_str(&format!(
        "  {label:<width$}  {pct:>6} {bar} {tokens:>tokens_w$}  {reqs:>reqs_w$} req",
        label = "total",
        width = width,
        pct = "100.0%",
        bar = " ".repeat(BAR_WIDTH),
        tokens = fmt_tokens(total),
        tokens_w = max_tokens,
        reqs = snapshot.requests,
        reqs_w = max_reqs,
    ));
    out
}

fn label_of(row: &Row) -> String {
    if row.gateway_model == row.opencode_ref {
        format!("{} (direct ref)", row.opencode_ref)
    } else {
        format!("{} → {}", row.gateway_model, row.opencode_ref)
    }
}

fn percent(tokens: u64, total: u64) -> String {
    if total == 0 {
        return "-".to_string();
    }
    format!("{:.1}%", (tokens as f64) * 100.0 / (total as f64))
}

fn bar(tokens: u64, total: u64) -> String {
    if total == 0 {
        return " ".repeat(BAR_WIDTH);
    }
    let share = (tokens as f64) / (total as f64);
    let mut filled = (share * BAR_WIDTH as f64).round() as usize;
    filled = filled.min(BAR_WIDTH);
    if tokens > 0 && filled == 0 {
        filled = 1;
    }
    format!("{}{}", "█".repeat(filled), "░".repeat(BAR_WIDTH - filled))
}

/// `950`, `12.3K`, `2.45M` — two significant-ish units, enough for a table.
pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Atomic, owner-only write (0600), mirroring `daemon::write_private`.
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(body)?;
    }
    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(requests: u64, input: u64, output: u64) -> Totals {
        Totals {
            requests,
            input,
            output,
            cached: 0,
        }
    }

    #[test]
    fn snapshot_sums_window_and_sorts_by_tokens() {
        let stats = UsageStats::in_memory();
        stats.record_on(
            100,
            "claude-sonnet-5",
            "opencode-go/deepseek-v4.1-flash",
            &totals(2, 1000, 500),
        );
        stats.record_on(
            100,
            "claude-opus-5",
            "github-copilot/gpt-6-luna",
            &totals(1, 9000, 1000),
        );
        stats.record_on(
            99,
            "claude-sonnet-5",
            "opencode-go/deepseek-v4.1-flash",
            &totals(1, 100, 0),
        );
        // Day outside the 7-day window ending at 100.
        stats.record_on(
            90,
            "claude-haiku-4-5",
            "opencode-go/muse-spark-1.3",
            &totals(5, 50_000, 0),
        );
        let snap = stats.snapshot_ending(100, 7);
        assert_eq!(snap.tokens(), 1_500 + 10_000 + 100);
        assert_eq!(snap.requests, 4);
        assert_eq!(snap.rows.len(), 2);
        assert_eq!(snap.rows[0].gateway_model, "claude-opus-5");
        assert_eq!(snap.rows[1].gateway_model, "claude-sonnet-5");
    }

    #[test]
    fn window_is_inclusive_of_today_minus_six() {
        let stats = UsageStats::in_memory();
        stats.record_on(94, "a", "a", &totals(1, 1, 0)); // day 94 = 100 - 6: in
        stats.record_on(93, "b", "b", &totals(1, 1, 0)); // day 93 = 100 - 7: out
        let snap = stats.snapshot_ending(100, 7);
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].gateway_model, "a");
    }

    #[test]
    fn flush_round_trips_and_prunes_old_days() {
        let dir = std::env::temp_dir().join(format!("ocg-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("ocg.stats.json");
        let stats = UsageStats::open(path.clone());
        let now = today();
        stats.record_on(
            now,
            "claude-sonnet-5",
            "opencode-go/glm-5.3-flash",
            &totals(3, 300, 30),
        );
        stats.record_on(now.saturating_sub(30), "old", "old", &totals(1, 1, 1));
        assert!(stats.is_dirty());
        stats.flush().unwrap();
        assert!(!stats.is_dirty());

        let reopened = UsageStats::open(path.clone());
        let snap = reopened.snapshot(7);
        assert_eq!(
            snap.rows.len(),
            1,
            "30-day-old bucket must be pruned on open"
        );
        assert_eq!(snap.tokens(), 330);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_starts_empty_instead_of_failing() {
        let dir = std::env::temp_dir().join(format!("ocg-stats-bad-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ocg.stats.json");
        std::fs::write(&path, "{not json").unwrap();
        let stats = UsageStats::open(path.clone());
        assert_eq!(stats.snapshot(7).requests, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_shows_alias_conversion_and_direct_refs() {
        let stats = UsageStats::in_memory();
        stats.record_on(
            100,
            "claude-sonnet-5",
            "opencode-go/deepseek-v4.1-flash",
            &totals(3, 1_500_000, 950_000),
        );
        stats.record_on(
            100,
            "opencode-go/glm-5.3-flash",
            "opencode-go/glm-5.3-flash",
            &totals(1, 330_000, 0),
        );
        let snap = stats.snapshot_ending(100, 7);
        let text = render_report(&snap, 7);
        assert!(
            text.contains("Most used models · last 7 days · 2.78M tokens"),
            "{text}"
        );
        assert!(
            text.contains("claude-sonnet-5 → opencode-go/deepseek-v4.1-flash"),
            "{text}"
        );
        assert!(
            text.contains("opencode-go/glm-5.3-flash (direct ref)"),
            "{text}"
        );
        assert!(text.contains("100.0%"), "{text}");
        // Rows add up to the total (the percentages are shares of it).
        assert!(text.contains(" 2.45M"), "{text}");
        assert!(text.contains("330.0K"), "{text}");
    }

    #[test]
    fn render_says_when_nothing_was_recorded() {
        assert!(render_report(&Snapshot::default(), 7).contains("no usage recorded yet"));
    }

    #[test]
    fn render_without_token_usage_still_counts_requests() {
        let stats = UsageStats::in_memory();
        stats.record_on(100, "m", "p/m", &totals(4, 0, 0));
        let text = render_report(&stats.snapshot_ending(100, 7), 7);
        assert!(
            text.contains("no token usage reported (4 requests)"),
            "{text}"
        );
        assert!(text.contains("4 req"), "{text}");
    }

    #[test]
    fn formatter_units() {
        assert_eq!(fmt_tokens(950), "950");
        assert_eq!(fmt_tokens(12_340), "12.3K");
        assert_eq!(fmt_tokens(4_200_000), "4.20M");
    }
}
