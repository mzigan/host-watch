use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub hostname: Option<String>,
    pub kernel: Option<String>,
    pub uptime_seconds: Option<f64>,
    pub cpu_count: Option<u64>,
    pub cpu_total: Option<u64>,
    pub cpu_idle: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub memory_total_kb: Option<u64>,
    pub memory_available_kb: Option<u64>,
    pub memory_percent: Option<f64>,
    pub swap_total_kb: Option<u64>,
    pub swap_free_kb: Option<u64>,
    pub processes: Option<u64>,
    pub failed_units: Option<u64>,
    pub disks: Vec<Disk>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Disk {
    pub mount: String,
    pub total_kb: u64,
    pub available_kb: u64,
    pub used_percent: f64,
}
pub fn parse(output: &str) -> Result<Metrics> {
    let mut m = Metrics::default();
    let mut complete = false;
    for line in output.lines() {
        let Some((k, v)) = line.split_once('=') else {
            bail!("invalid collector line")
        };
        match k {
            "hostname" => m.hostname = Some(v.into()),
            "kernel" => m.kernel = Some(v.into()),
            "uptime_seconds" => m.uptime_seconds = Some(v.parse()?),
            "cpu_count" => m.cpu_count = Some(v.parse()?),
            "cpu_total" => m.cpu_total = Some(v.parse()?),
            "cpu_idle" => m.cpu_idle = Some(v.parse()?),
            "load1" => m.load1 = Some(v.parse()?),
            "load5" => m.load5 = Some(v.parse()?),
            "load15" => m.load15 = Some(v.parse()?),
            "memory_total_kb" => m.memory_total_kb = Some(v.parse()?),
            "memory_available_kb" => m.memory_available_kb = Some(v.parse()?),
            "swap_total_kb" => m.swap_total_kb = Some(v.parse()?),
            "swap_free_kb" => m.swap_free_kb = Some(v.parse()?),
            "processes" => m.processes = Some(v.parse()?),
            "failed_units" => m.failed_units = Some(v.parse()?),
            "collector_complete" => complete = v == "1",
            "disk" => {
                let p: Vec<_> = v.splitn(4, '\t').collect();
                ensure!(p.len() == 4, "invalid disk record");
                let d = Disk {
                    total_kb: p[0].parse()?,
                    available_kb: p[1].parse()?,
                    used_percent: p[2].parse()?,
                    mount: p[3].into(),
                };
                ensure!(
                    d.used_percent.is_finite() && (0.0..=100.0).contains(&d.used_percent),
                    "invalid disk percentage"
                );
                m.disks.push(d);
            }
            _ => {}
        }
    }
    ensure!(complete, "incomplete collector response");
    for v in [m.uptime_seconds, m.load1, m.load5, m.load15]
        .into_iter()
        .flatten()
    {
        ensure!(v.is_finite() && v >= 0., "invalid numeric metric");
    }
    if let (Some(total), Some(avail)) = (m.memory_total_kb, m.memory_available_kb) {
        ensure!(total > 0 && avail <= total, "invalid memory counters");
        m.memory_percent = Some(100. * (total - avail) as f64 / total as f64);
    }
    Ok(m)
}
impl Metrics {
    pub fn derive_cpu(&mut self, previous: Option<&Self>) {
        if let Some(p) = previous {
            if matches!((self.uptime_seconds,p.uptime_seconds),(Some(a),Some(b)) if a<=b) {
                return;
            }
            if let (Some(t), Some(i), Some(pt), Some(pi)) =
                (self.cpu_total, self.cpu_idle, p.cpu_total, p.cpu_idle)
                && let (Some(dt), Some(di)) = (t.checked_sub(pt), i.checked_sub(pi))
                && dt > 0
                && di <= dt
            {
                self.cpu_percent = Some(100. * (dt - di) as f64 / dt as f64);
            }
        }
    }
}
