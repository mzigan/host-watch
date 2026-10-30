use crate::{health::Health, metrics::Metrics};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
};
#[derive(Default, Serialize, Deserialize)]
pub struct State {
    pub hosts: BTreeMap<String, HostState>,
}
#[derive(Default, Serialize, Deserialize)]
pub struct HostState {
    pub last_poll: Option<i64>,
    pub last_success: Option<i64>,
    pub previous_success: Option<i64>,
    pub health: Health,
    pub failures: u64,
    pub metrics: Option<Metrics>,
    pub latency_ms: Option<u64>,
    pub cpu_high_since: Option<i64>,
    pub reasons: Vec<String>,
    pub error: Option<String>,
}
pub fn read(dir: &Path) -> Result<State> {
    match std::fs::read(dir.join("state.json")) {
        Ok(b) => Ok(serde_json::from_slice(&b)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e.into()),
    }
}
pub fn write(dir: &Path, s: &State) -> Result<()> {
    let path = dir.join("state.json.tmp");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    f.write_all(&serde_json::to_vec_pretty(s)?)?;
    f.sync_all()?;
    std::fs::rename(path, dir.join("state.json"))?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
