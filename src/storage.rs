use crate::{metrics::Metrics, state::State};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags, params};
use std::path::Path;
pub struct Storage(Connection);
const NEW_COLUMNS: [&str; 4] = [
    "rx_bytes_per_sec",
    "tx_bytes_per_sec",
    "cpu_temperature_c",
    "max_disk_used_percent",
];
fn columns(c: &Connection) -> Result<Vec<String>> {
    Ok(c.prepare("PRAGMA table_info(snapshots)")?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
impl Storage {
    pub fn open(dir: &Path) -> Result<Self> {
        let mut c = Connection::open(dir.join("history.db"))?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS snapshots (timestamp INTEGER NOT NULL, host TEXT NOT NULL, health TEXT NOT NULL, metrics_json TEXT, latency_ms INTEGER, stale INTEGER NOT NULL, PRIMARY KEY(host,timestamp)); CREATE INDEX IF NOT EXISTS snapshots_time ON snapshots(timestamp);")?;
        let existing = columns(&c)?;
        let tx = c.transaction()?;
        for column in NEW_COLUMNS {
            if !existing.iter().any(|name| name == column) {
                tx.execute_batch(&format!("ALTER TABLE snapshots ADD COLUMN {column} REAL"))?;
            }
        }
        tx.commit()?;
        Ok(Self(c))
    }
    pub fn append(&mut self, state: &State, now: i64, days: u64) -> Result<()> {
        let tx = self.0.transaction()?;
        for (host, s) in &state.hosts {
            let m = s.metrics.as_ref().filter(|_| s.failures == 0);
            tx.execute(
                "INSERT OR REPLACE INTO snapshots (timestamp,host,health,metrics_json,latency_ms,stale,rx_bytes_per_sec,tx_bytes_per_sec,cpu_temperature_c,max_disk_used_percent) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![now,host,s.health.to_string(),m.map(serde_json::to_string).transpose()?,
                    if s.failures==0 {s.latency_ms}else{None},s.failures>0,
                    m.and_then(|m|m.rx_bytes_per_sec),m.and_then(|m|m.tx_bytes_per_sec),m.and_then(|m|m.cpu_temperature_c),m.and_then(Metrics::max_disk_used_percent)],
            )?;
        }
        tx.execute(
            "DELETE FROM snapshots WHERE timestamp < ?1",
            [now.saturating_sub((days * 86400) as i64)],
        )?;
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub fn count(&self) -> i64 {
        self.0
            .query_row("SELECT COUNT(*) FROM snapshots", [], |r| r.get(0))
            .unwrap()
    }
}

/// CPU, RAM, load1, SSH ms, max disk %, RX B/s, TX B/s.
/// Keep sample times and explicit failures separate from empty drawing buckets.
#[derive(Clone, Debug, Default)]
pub struct Bucket {
    pub values: [Option<f64>; 7],
    pub first_at: [Option<i64>; 7],
    pub last_at: [Option<i64>; 7],
    pub break_before: [bool; 7],
    minima: [Option<f64>; 7],
    maxima: [Option<f64>; 7],
    sums: [f64; 7],
    counts: [u64; 7],
}
impl Bucket {
    pub fn extrema(buckets: &[Self], index: usize) -> Option<(f64, f64)> {
        Some((
            buckets
                .iter()
                .filter_map(|b| b.minima[index])
                .reduce(f64::min)?,
            buckets
                .iter()
                .filter_map(|b| b.maxima[index])
                .reduce(f64::max)?,
        ))
    }
    /// Mean of actual samples, weighted by sample count rather than bucket width.
    pub fn average(buckets: &[Self], index: usize) -> Option<f64> {
        let sum: f64 = buckets.iter().map(|b| b.sums[index]).sum();
        let count: u64 = buckets.iter().map(|b| b.counts[index]).sum();
        (count > 0).then(|| sum / count as f64)
    }
    fn add(&mut self, values: [Option<f64>; 7], timestamp: i64, pending_gap: &mut [bool; 7]) {
        for (i, value) in values.into_iter().enumerate() {
            if let Some(v) = value.filter(|v| v.is_finite()) {
                self.first_at[i].get_or_insert(timestamp);
                self.last_at[i] = Some(timestamp);
                self.break_before[i] |= pending_gap[i];
                pending_gap[i] = false;
                self.minima[i] = Some(self.minima[i].map_or(v, |old| old.min(v)));
                self.maxima[i] = Some(self.maxima[i].map_or(v, |old| old.max(v)));
                self.sums[i] += v;
                self.counts[i] += 1;
                self.values[i] = if i == 4 {
                    Some(self.values[i].map_or(v, |old| old.max(v)))
                } else {
                    Some(self.sums[i] / self.counts[i] as f64)
                };
            } else {
                pending_gap[i] = true;
            }
        }
    }
}
/// Read only the requested host/time range, streaming rows into bounded buckets.
/// A reader never creates a DB or runs migrations. Legacy JSON is also supported.
pub fn history(dir: &Path, host: &str, start: i64, end: i64, width: usize) -> Result<Vec<Bucket>> {
    let c = Connection::open_with_flags(
        dir.join("history.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.busy_timeout(std::time::Duration::from_millis(100))?;
    let existing = columns(&c)?;
    let select = NEW_COLUMNS
        .iter()
        .map(|name| {
            if existing.iter().any(|s| s == name) {
                *name
            } else {
                "NULL"
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt=c.prepare(&format!("SELECT timestamp,metrics_json,latency_ms,stale,{select} FROM snapshots WHERE host=?1 AND timestamp>=?2 AND timestamp<=?3 ORDER BY timestamp"))?;
    let mut rows = stmt.query(params![host, start, end])?;
    let mut buckets = vec![Bucket::default(); width.clamp(1, 1024)];
    let span = end.saturating_sub(start).max(1) as f64;
    let mut pending_gap = [false; 7];
    while let Some(row) = rows.next()? {
        let timestamp: i64 = row.get(0)?;
        let index = (((timestamp.saturating_sub(start) as f64 / span) * buckets.len() as f64)
            as usize)
            .min(buckets.len() - 1);
        if row.get::<_, bool>(3)? {
            pending_gap.fill(true);
            continue;
        }
        let json: Option<String> = row.get(1)?;
        let m = json
            .as_deref()
            .map(serde_json::from_str::<Metrics>)
            .transpose()?;
        let latency: Option<f64> = row.get(2)?;
        let disk: Option<f64> = row.get(7)?;
        let rx: Option<f64> = row.get(4)?;
        let tx: Option<f64> = row.get(5)?;
        buckets[index].add(
            [
                m.as_ref().and_then(|m| m.cpu_percent),
                m.as_ref().and_then(|m| m.memory_percent),
                m.as_ref().and_then(|m| m.load1),
                latency,
                disk.or_else(|| m.as_ref().and_then(Metrics::max_disk_used_percent)),
                rx.or_else(|| m.as_ref().and_then(|m| m.rx_bytes_per_sec)),
                tx.or_else(|| m.as_ref().and_then(|m| m.tx_bytes_per_sec)),
            ],
            timestamp,
            &mut pending_gap,
        );
    }
    Ok(buckets)
}
