use crate::{config::Thresholds, state::HostState};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Health {
    Ok,
    Warning,
    Critical,
    #[default]
    Unknown,
}
impl std::fmt::Display for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Ok => "OK",
                Self::Warning => "WARNING",
                Self::Critical => "CRITICAL",
                Self::Unknown => "UNKNOWN",
            }
        )
    }
}
pub fn evaluate(s: &mut HostState, t: &Thresholds, now: i64, interval: u64) {
    s.reasons.clear();
    if s.failures > 0 {
        s.health = if s.failures >= 3 {
            Health::Critical
        } else {
            Health::Unknown
        };
        s.cpu_high_since = None;
        s.reasons.push(format!(
            "SSH/collector failed {} polls: {}",
            s.failures,
            s.error.as_deref().unwrap_or("unknown")
        ));
        return;
    }
    let Some(m) = &s.metrics else {
        s.health = Health::Unknown;
        return;
    };
    s.health = Health::Ok;
    let continuity = s
        .last_success
        .zip(s.previous_success)
        .is_some_and(|(a, b)| a - b <= (interval.saturating_mul(2)).min(i64::MAX as u64) as i64);
    if m.cpu_percent.is_some_and(|v| v > t.cpu_warning) {
        if !continuity {
            s.cpu_high_since = None;
        }
        let start = *s.cpu_high_since.get_or_insert(now);
        if now - start >= 300 {
            s.health = Health::Warning;
            s.reasons
                .push(format!("CPU above {}% for 5 minutes", t.cpu_warning));
        }
    } else {
        s.cpu_high_since = None;
    }
    if let Some(v) = m.memory_percent
        && v > t.memory_warning
    {
        s.health = Health::Warning;
        s.reasons.push(format!("memory={v:.1}%"));
    }
    if let (Some(load), Some(cpus)) = (m.load1, m.cpu_count)
        && cpus > 0
        && load / cpus as f64 > t.load_warning_per_cpu
    {
        s.health = Health::Warning;
        s.reasons.push(format!("load1={load:.2} ({cpus} CPUs)"));
    }
    if m.failed_units.is_some_and(|v| v > 0) {
        s.health = Health::Warning;
        s.reasons
            .push(format!("failed units={}", m.failed_units.unwrap()));
    }
    for d in &m.disks {
        if d.used_percent > t.disk_warning {
            if s.health != Health::Critical {
                s.health = Health::Warning;
            }
            s.reasons
                .push(format!("disk[{}]={:.0}%", d.mount, d.used_percent));
        }
        if d.used_percent > t.disk_critical {
            s.health = Health::Critical;
        }
    }
    if m.memory_percent.is_some_and(|v| v > t.memory_critical) {
        s.health = Health::Critical;
    }
    if let Some(temp) = m.cpu_temperature_c
        && temp > t.temperature_warning
    {
        if temp > t.temperature_critical {
            s.health = Health::Critical;
        } else if s.health != Health::Critical {
            s.health = Health::Warning;
        }
        s.reasons.push(format!("CPU temperature={temp:.1} C"));
    }
    if s.health == Health::Ok
        && (m.cpu_percent.is_none()
            || m.memory_percent.is_none()
            || m.disks.is_empty()
            || m.load1.is_none()
            || (m.failed_units.is_none() && m.systemd_supported != Some(false)))
    {
        s.health = Health::Unknown;
        s.reasons.push(
            "insufficient metrics (CPU needs two successful snapshots; available collectors may have failed)"
                .into(),
        );
    }
}
