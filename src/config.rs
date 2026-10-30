use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub poll_interval_seconds: u64,
    pub history_days: u64,
    pub ssh_timeout_seconds: u64,
    pub max_parallel_hosts: usize,
    pub data_dir: PathBuf,
    pub thresholds: Thresholds,
    pub hosts: Vec<Host>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval_seconds: 60,
            history_days: 30,
            ssh_timeout_seconds: 10,
            max_parallel_hosts: 4,
            data_dir: "/var/lib/host-watch".into(),
            thresholds: Thresholds::default(),
            hosts: vec![],
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub name: String,
    pub ssh_host: Option<String>,
    #[serde(default = "enabled")]
    pub enabled: bool,
}
fn enabled() -> bool {
    true
}
impl Host {
    pub fn target(&self) -> &str {
        self.ssh_host.as_deref().unwrap_or(&self.name)
    }
}
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    pub cpu_warning: f64,
    pub temperature_warning: f64,
    pub temperature_critical: f64,
    pub memory_warning: f64,
    pub memory_critical: f64,
    pub disk_warning: f64,
    pub disk_critical: f64,
    pub load_warning_per_cpu: f64,
}
impl Default for Thresholds {
    fn default() -> Self {
        Self {
            cpu_warning: 90.,
            temperature_warning: 80.,
            temperature_critical: 90.,
            memory_warning: 90.,
            memory_critical: 98.,
            disk_warning: 85.,
            disk_critical: 95.,
            load_warning_per_cpu: 1.5,
        }
    }
}
impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        let c: Self = toml::from_str(
            &std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?,
        )?;
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.poll_interval_seconds > 0 && self.ssh_timeout_seconds > 0 && self.history_days > 0,
            "interval, timeout and retention must be positive"
        );
        ensure!(
            (1..=128).contains(&self.max_parallel_hosts),
            "max_parallel_hosts must be 1..128"
        );
        ensure!(self.history_days <= 36500, "history_days must be <= 36500");
        let t = &self.thresholds;
        for v in [
            t.cpu_warning,
            t.memory_warning,
            t.memory_critical,
            t.disk_warning,
            t.disk_critical,
        ] {
            ensure!(
                v.is_finite() && (0.0..=100.0).contains(&v),
                "threshold must be 0..100"
            );
        }
        ensure!(
            t.memory_warning < t.memory_critical
                && t.disk_warning < t.disk_critical
                && t.load_warning_per_cpu.is_finite()
                && t.load_warning_per_cpu > 0.,
            "invalid threshold ordering/load threshold"
        );
        ensure!(
            t.temperature_warning.is_finite()
                && t.temperature_critical.is_finite()
                && (-20.0..=150.0).contains(&t.temperature_warning)
                && (-20.0..=150.0).contains(&t.temperature_critical)
                && t.temperature_warning < t.temperature_critical,
            "invalid temperature thresholds"
        );
        let mut names = HashSet::new();
        for h in &self.hosts {
            ensure!(
                !h.name.trim().is_empty() && names.insert(&h.name),
                "host names must be nonempty and unique"
            );
            ensure!(
                !h.target().is_empty()
                    && !h.target().starts_with('-')
                    && !h.target().chars().any(char::is_whitespace),
                "invalid SSH target"
            );
        }
        Ok(())
    }
}
