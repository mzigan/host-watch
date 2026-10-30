use crate::state::State;
use anyhow::Result;
use rusqlite::{Connection, params};
use std::path::Path;
pub struct Storage(Connection);
impl Storage {
    pub fn open(dir: &Path) -> Result<Self> {
        let c = Connection::open(dir.join("history.db"))?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS snapshots (timestamp INTEGER NOT NULL, host TEXT NOT NULL, health TEXT NOT NULL, metrics_json TEXT, latency_ms INTEGER, stale INTEGER NOT NULL, PRIMARY KEY(host,timestamp)); CREATE INDEX IF NOT EXISTS snapshots_time ON snapshots(timestamp);")?;
        Ok(Self(c))
    }
    pub fn append(&mut self, state: &State, now: i64, days: u64) -> Result<()> {
        let tx = self.0.transaction()?;
        for (host, s) in &state.hosts {
            tx.execute(
                "INSERT OR REPLACE INTO snapshots VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    now,
                    host,
                    s.health.to_string(),
                    if s.failures == 0 {
                        s.metrics.as_ref().map(serde_json::to_string).transpose()?
                    } else {
                        None
                    },
                    if s.failures == 0 { s.latency_ms } else { None },
                    s.failures > 0
                ],
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
