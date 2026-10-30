use crate::{config::*, health::*, metrics::*, state::*, storage::Storage};
fn sample() -> Metrics {
    let mut m = crate::metrics::parse(include_str!("../tests/fixtures/collector.txt")).unwrap();
    m.cpu_percent = Some(10.);
    m
}
fn host() -> HostState {
    HostState {
        metrics: Some(sample()),
        last_success: Some(1000),
        previous_success: Some(940),
        ..Default::default()
    }
}
fn eval(h: &mut HostState, t: i64) {
    evaluate(h, &Thresholds::default(), t, 60);
}
#[test]
fn collector_parsing() {
    let m = sample();
    assert_eq!(m.hostname.as_deref(), Some("fixture"));
    assert_eq!(m.disks[0].mount, "/");
    assert_eq!(m.memory_percent, Some(60.));
}
#[test]
fn missing_is_unknown() {
    let m = crate::metrics::parse("collector_complete=1\n").unwrap();
    assert!(m.memory_percent.is_none());
    let mut h = HostState {
        metrics: Some(m),
        ..Default::default()
    };
    eval(&mut h, 0);
    assert_eq!(h.health, Health::Unknown);
    assert!(crate::metrics::parse("cpu_count=4\n").is_err());
}
#[test]
fn failures_and_recovery() {
    let mut h = host();
    for n in 1..=3 {
        h.failures = n;
        eval(&mut h, 1000);
        assert_eq!(
            h.health,
            if n < 3 {
                Health::Unknown
            } else {
                Health::Critical
            }
        );
        assert!(h.metrics.is_some());
    }
    h.failures = 0;
    eval(&mut h, 1060);
    assert_eq!(h.health, Health::Ok);
}
#[test]
fn sustained_cpu() {
    let mut h = host();
    h.metrics.as_mut().unwrap().cpu_percent = Some(95.);
    eval(&mut h, 1000);
    assert_eq!(h.health, Health::Ok);
    eval(&mut h, 1299);
    assert_eq!(h.health, Health::Ok);
    eval(&mut h, 1300);
    assert_eq!(h.health, Health::Warning);
    h.failures = 1;
    eval(&mut h, 1360);
    h.failures = 0;
    eval(&mut h, 1420);
    assert_eq!(h.health, Health::Ok);
}
#[test]
fn disk_thresholds() {
    for (v, health) in [(86., Health::Warning), (96., Health::Critical)] {
        let mut h = host();
        h.metrics.as_mut().unwrap().disks[0].used_percent = v;
        eval(&mut h, 1000);
        assert_eq!(h.health, health);
    }
}
#[test]
fn memory_critical() {
    let mut h = host();
    h.metrics.as_mut().unwrap().memory_percent = Some(99.);
    eval(&mut h, 1000);
    assert_eq!(h.health, Health::Critical);
}
#[test]
fn cpu_deltas_and_reboot() {
    let prev = sample();
    let mut m = sample();
    m.uptime_seconds = Some(1060.);
    m.cpu_total = Some(1200);
    m.cpu_idle = Some(900);
    m.cpu_percent = None;
    m.derive_cpu(Some(&prev));
    assert_eq!(m.cpu_percent, Some(50.));
    m.cpu_percent = None;
    m.uptime_seconds = Some(10.);
    m.derive_cpu(Some(&prev));
    assert_eq!(m.cpu_percent, None);
}
#[test]
fn atomic_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = State::default();
    s.hosts.insert("msk1".into(), host());
    write(dir.path(), &s).unwrap();
    write(dir.path(), &s).unwrap();
    assert_eq!(
        read(dir.path()).unwrap().hosts["msk1"]
            .metrics
            .as_ref()
            .unwrap()
            .hostname
            .as_deref(),
        Some("fixture")
    );
    assert!(!dir.path().join("state.json.tmp").exists());
}
#[test]
fn history_retention() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Storage::open(dir.path()).unwrap();
    let mut s = State::default();
    s.hosts.insert("msk1".into(), host());
    db.append(&s, 100, 1).unwrap();
    assert_eq!(db.count(), 1);
    db.append(&s, 100000, 1).unwrap();
    assert_eq!(db.count(), 1);
}
#[test]
fn summary_counts() {
    let mut s = State::default();
    for (i, health) in [
        Health::Ok,
        Health::Warning,
        Health::Critical,
        Health::Unknown,
    ]
    .into_iter()
    .enumerate()
    {
        s.hosts.insert(
            i.to_string(),
            HostState {
                health,
                ..Default::default()
            },
        );
    }
    assert_eq!(crate::counts(&s), [1, 1, 1, 1]);
}
#[test]
fn alias_and_identity() {
    let c: Config = toml::from_str("[[hosts]]\nname='msk1'\n").unwrap();
    c.validate().unwrap();
    let args = crate::ssh::arguments(&c.hosts[0], 10);
    assert_eq!(args[args.len() - 2], "msk1");
    assert!(args.iter().any(|a| a == "StrictHostKeyChecking=yes"));
    let changed = Host {
        name: "msk1".into(),
        ssh_host: Some("192.0.2.1".into()),
        enabled: true,
    };
    assert_eq!(changed.name, c.hosts[0].name);
    assert_eq!(changed.target(), "192.0.2.1");
}
#[test]
fn invalid_config() {
    let c: Config = toml::from_str("max_parallel_hosts=0").unwrap();
    assert!(c.validate().is_err());
    let c: Config = toml::from_str("[[hosts]]\nname='-oProxyCommand=bad'").unwrap();
    assert!(c.validate().is_err());
}
#[tokio::test]
async fn unreachable_host() {
    let h = Host {
        name: "bad".into(),
        ssh_host: Some("127.0.0.1".into()),
        enabled: true,
    }; // Test invocation without contacting even localhost.
    let args = crate::ssh::arguments(&h, 1);
    let output = tokio::process::Command::new("/usr/bin/ssh")
        .args(["-F", "/dev/null", "-o", "ProxyCommand=false"])
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
}
#[test]
fn collector_runs_locally() {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(include_str!("collector.sh"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let m = crate::metrics::parse(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    assert!(m.cpu_count.unwrap() > 0);
    assert!(m.memory_total_kb.unwrap() > 0);
}

#[test]
fn history_query_and_failed_sample() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Storage::open(dir.path()).unwrap();
    let mut s = State::default();
    s.hosts.insert("stable-alias".into(), host());
    db.append(&s, 1000, 30).unwrap();
    s.hosts.get_mut("stable-alias").unwrap().failures = 1;
    db.append(&s, 1060, 30).unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("history.db")).unwrap();
    let (name, json): (String, String) = conn
        .query_row(
            "SELECT host, metrics_json FROM snapshots WHERE timestamp=1000",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(name, "stable-alias");
    let m: Metrics = serde_json::from_str(&json).unwrap();
    assert_eq!(m.load1, Some(0.42));
    let (json, latency, stale): (Option<String>, Option<i64>, bool) = conn
        .query_row(
            "SELECT metrics_json, latency_ms, stale FROM snapshots WHERE timestamp=1060",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(json.is_none() && latency.is_none() && stale);
}

#[test]
fn cpu_gap_resets_duration() {
    let mut h = host();
    h.metrics.as_mut().unwrap().cpu_percent = Some(99.);
    eval(&mut h, 1000);
    h.previous_success = Some(1000);
    h.last_success = Some(1500);
    eval(&mut h, 1500);
    assert_eq!(h.health, Health::Ok);
    assert_eq!(h.cpu_high_since, Some(1500));
}
