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
    pub systemd_supported: Option<bool>,
    pub disks: Vec<Disk>,
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
    pub rx_bytes_per_sec: Option<f64>,
    pub tx_bytes_per_sec: Option<f64>,
    pub cpu_temperature_c: Option<f64>,
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
    let mut net = String::new();
    let mut sensors = String::new();
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
            "systemd_supported" => {
                m.systemd_supported = Some(match v {
                    "0" => false,
                    "1" => true,
                    _ => bail!("invalid systemd support marker"),
                })
            }
            "collector_complete" => complete = v == "1",
            "net" => {
                net.push_str(v);
                net.push('\n');
            }
            "sensor" => {
                sensors.push_str(v);
                sensors.push('\n');
            }
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
    if !net.is_empty() {
        let (rx, tx) = parse_net_dev(&net)?;
        m.rx_bytes = Some(rx);
        m.tx_bytes = Some(tx);
    }
    m.cpu_temperature_c = parse_temperature(&sensors);
    Ok(m)
}
impl Metrics {
    pub fn failed_units_display(&self) -> String {
        self.failed_units.map_or_else(
            || {
                if self.systemd_supported == Some(false) {
                    "unsupported".into()
                } else {
                    "n/a".into()
                }
            },
            |n| n.to_string(),
        )
    }
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

/// Aggregate all non-loopback interfaces. Headers are accepted for fixtures.
pub fn parse_net_dev(raw: &str) -> Result<(u64, u64)> {
    let (mut rx, mut tx) = (0_u64, 0_u64);
    let mut records = 0;
    for line in raw.lines() {
        let Some((interface, values)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<_> = values.split_whitespace().collect();
        ensure!(fields.len() == 16, "invalid /proc/net/dev record");
        records += 1;
        if interface.trim() == "lo" {
            continue;
        }
        rx = rx
            .checked_add(fields[0].parse()?)
            .ok_or_else(|| anyhow::anyhow!("RX counter overflow"))?;
        tx = tx
            .checked_add(fields[8].parse()?)
            .ok_or_else(|| anyhow::anyhow!("TX counter overflow"))?;
    }
    ensure!(records > 0, "no /proc/net/dev records");
    Ok((rx, tx))
}

/// Only standard CPU labels from `sensors -u`; never guess from generic temp1.
pub fn parse_temperature(raw: &str) -> Option<f64> {
    let mut groups: [Vec<f64>; 3] = Default::default();
    let mut group = None;
    for line in raw.lines() {
        let value = line.trim();
        if !line.starts_with(char::is_whitespace) {
            group = if value.starts_with("Package id ") && value.ends_with(':') {
                Some(0)
            } else if value == "Tdie:" {
                Some(1)
            } else if value == "Tctl:" {
                Some(2)
            } else {
                None
            };
        }
        if let Some(g) = group
            && let Some((key, value)) = value.split_once(':')
            && key.starts_with("temp")
            && key.ends_with("_input")
            && let Ok(temp) = value.trim().parse::<f64>()
            && temp.is_finite()
            && (-20.0..=150.0).contains(&temp)
        {
            groups[g].push(temp);
        }
    }
    // Multiple CPU packages are represented by their maximum temperature.
    groups
        .iter()
        .find_map(|g| g.iter().copied().reduce(f64::max))
}

impl Metrics {
    pub fn derive_network(&mut self, previous: Option<&Self>) {
        self.rx_bytes_per_sec = None;
        self.tx_bytes_per_sec = None;
        let Some(p) = previous else { return };
        let Some(elapsed) = self
            .uptime_seconds
            .zip(p.uptime_seconds)
            .map(|(a, b)| a - b)
            .filter(|v| *v > 0.)
        else {
            return;
        };
        self.rx_bytes_per_sec = self
            .rx_bytes
            .zip(p.rx_bytes)
            .and_then(|(a, b)| a.checked_sub(b))
            .map(|delta| delta as f64 / elapsed);
        self.tx_bytes_per_sec = self
            .tx_bytes
            .zip(p.tx_bytes)
            .and_then(|(a, b)| a.checked_sub(b))
            .map(|delta| delta as f64 / elapsed);
    }
    pub fn max_disk_used_percent(&self) -> Option<f64> {
        self.disks.iter().map(|d| d.used_percent).reduce(f64::max)
    }
}
