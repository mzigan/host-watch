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

#[test]
fn network_parser_excludes_loopback() {
    let raw = include_str!("../tests/fixtures/net-dev.txt");
    assert_eq!(parse_net_dev(raw).unwrap(), (1300, 2400));
    let output = raw
        .lines()
        .map(|line| format!("net={line}\n"))
        .collect::<String>()
        + "collector_complete=1\n";
    let m = crate::metrics::parse(&output).unwrap();
    assert_eq!((m.rx_bytes, m.tx_bytes), (Some(1300), Some(2400)));
    assert!(parse_net_dev("eth0: bad").is_err());
}
#[test]
fn network_deltas_first_reset_and_reboot() {
    let previous = Metrics {
        rx_bytes: Some(1000),
        tx_bytes: Some(2000),
        uptime_seconds: Some(100.),
        ..Default::default()
    };
    let mut current = Metrics {
        rx_bytes: Some(1600),
        tx_bytes: Some(3200),
        uptime_seconds: Some(160.),
        ..Default::default()
    };
    current.derive_network(None);
    assert_eq!(
        (current.rx_bytes_per_sec, current.tx_bytes_per_sec),
        (None, None)
    );
    current.derive_network(Some(&previous));
    assert_eq!(
        (current.rx_bytes_per_sec, current.tx_bytes_per_sec),
        (Some(10.), Some(20.))
    );
    current.rx_bytes = Some(500);
    current.tx_bytes = Some(1000);
    current.derive_network(Some(&previous));
    assert_eq!(
        (current.rx_bytes_per_sec, current.tx_bytes_per_sec),
        (None, None)
    );
    current.rx_bytes = Some(9999);
    current.tx_bytes = Some(9999);
    current.uptime_seconds = Some(1.);
    current.derive_network(Some(&previous));
    assert_eq!(
        (current.rx_bytes_per_sec, current.tx_bytes_per_sec),
        (None, None)
    );
}
#[test]
fn temperature_parser_optional_and_cpu_only() {
    assert_eq!(
        parse_temperature(include_str!("../tests/fixtures/sensors.txt")),
        Some(53.)
    );
    assert_eq!(
        parse_temperature("Tctl:\n  temp1_input: 85.0\nTdie:\n  temp2_input: 75.0"),
        Some(75.)
    );
    for raw in [
        "",
        "acpitz-acpi-0\ntemp1:\n  temp1_input: 70.0",
        "Package id 0:\n  temp1_input: NaN",
        "Tctl:\n  temp1_input: 999",
    ] {
        assert_eq!(parse_temperature(raw), None);
    }
    let m = crate::metrics::parse("collector_complete=1\n").unwrap();
    assert!(m.cpu_temperature_c.is_none());
}
#[test]
fn temperature_thresholds_and_network_does_not_change_health() {
    let mut h = host();
    for (temp, expected) in [
        (None, Health::Ok),
        (Some(81.), Health::Warning),
        (Some(91.), Health::Critical),
    ] {
        let m = h.metrics.as_mut().unwrap();
        m.cpu_temperature_c = temp;
        m.rx_bytes_per_sec = Some(1e12);
        m.tx_bytes_per_sec = Some(1e12);
        eval(&mut h, 1000);
        assert_eq!(h.health, expected);
    }
    h.metrics.as_mut().unwrap().disks[0].used_percent = 96.;
    h.metrics.as_mut().unwrap().cpu_temperature_c = Some(81.);
    eval(&mut h, 1000);
    assert_eq!(h.health, Health::Critical);
}
#[test]
fn old_config_and_state_remain_compatible() {
    let c: Config = toml::from_str("[thresholds]\ncpu_warning=90\n").unwrap();
    c.validate().unwrap();
    assert_eq!(c.thresholds.temperature_warning, 80.);
    let mut json = serde_json::to_value(sample()).unwrap();
    for field in [
        "rx_bytes",
        "tx_bytes",
        "rx_bytes_per_sec",
        "tx_bytes_per_sec",
        "cpu_temperature_c",
    ] {
        json.as_object_mut().unwrap().remove(field);
    }
    let m: Metrics = serde_json::from_value(json).unwrap();
    assert!(m.cpu_temperature_c.is_none());
    assert!(m.rx_bytes_per_sec.is_none());
}
#[test]
fn sqlite_legacy_migration_and_reader() {
    let tmp = tempfile::tempdir().unwrap();
    let c = rusqlite::Connection::open(tmp.path().join("history.db")).unwrap();
    c.execute_batch("CREATE TABLE snapshots(timestamp INTEGER,host TEXT,health TEXT,metrics_json TEXT,latency_ms INTEGER,stale INTEGER,PRIMARY KEY(host,timestamp));").unwrap();
    c.execute(
        "INSERT INTO snapshots VALUES (1000,'legacy','OK',?1,42,0)",
        [serde_json::to_string(&sample()).unwrap()],
    )
    .unwrap();
    let before = crate::storage::history(tmp.path(), "legacy", 0, 2000, 10).unwrap();
    assert_eq!(before[5].values[0], Some(10.));
    assert_eq!(before[5].values[4], Some(61.));
    let db = Storage::open(tmp.path()).unwrap();
    assert_eq!(db.count(), 1);
    drop(db);
    assert_eq!(Storage::open(tmp.path()).unwrap().count(), 1);
    let columns: Vec<String> = c
        .prepare("PRAGMA table_info(snapshots)")
        .unwrap()
        .query_map([], |r| r.get(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for name in [
        "rx_bytes_per_sec",
        "tx_bytes_per_sec",
        "cpu_temperature_c",
        "max_disk_used_percent",
    ] {
        assert!(columns.iter().any(|c| c == name));
    }
}
#[test]
fn history_new_fields_and_time_ranges() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = Storage::open(tmp.path()).unwrap();
    let mut s = State::default();
    let mut h = host();
    let m = h.metrics.as_mut().unwrap();
    m.rx_bytes_per_sec = Some(100.);
    m.tx_bytes_per_sec = Some(200.);
    m.cpu_temperature_c = Some(50.);
    s.hosts.insert("a".into(), h);
    let now = 1_000_000;
    for age in [60, 7200, 172800, 691200] {
        db.append(&s, now - age, 30).unwrap();
    }
    for (range, expected) in [
        (crate::tui::Range::Hour, 1),
        (crate::tui::Range::Day, 2),
        (crate::tui::Range::Week, 3),
    ] {
        let buckets =
            crate::storage::history(tmp.path(), "a", now - range.seconds(), now, 100).unwrap();
        assert_eq!(
            buckets.iter().filter(|b| b.values[0].is_some()).count(),
            expected
        );
        assert!(
            buckets
                .iter()
                .any(|b| b.values[5] == Some(100.) && b.values[6] == Some(200.))
        );
    }
    let temp: f64 = rusqlite::Connection::open(tmp.path().join("history.db"))
        .unwrap()
        .query_row("SELECT cpu_temperature_c FROM snapshots LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(temp, 50.);
    assert!(
        crate::storage::history(tmp.path(), "other", 0, now, 10)
            .unwrap()
            .iter()
            .all(|b| b.values == [None; 7])
    );
}
#[test]
fn downsampling_averages_max_disk_and_gaps() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = Storage::open(tmp.path()).unwrap();
    let mut s = State::default();
    s.hosts.insert("a".into(), host());
    db.append(&s, 10, 30).unwrap();
    let h = s.hosts.get_mut("a").unwrap();
    let m = h.metrics.as_mut().unwrap();
    m.cpu_percent = Some(30.);
    m.disks[0].used_percent = 90.;
    db.append(&s, 20, 30).unwrap();
    s.hosts.get_mut("a").unwrap().failures = 1;
    db.append(&s, 150, 30).unwrap();
    let buckets = crate::storage::history(tmp.path(), "a", 0, 300, 3).unwrap();
    assert_eq!(buckets.len(), 3);
    assert_eq!(buckets[0].values[0], Some(20.));
    assert_eq!(buckets[0].values[4], Some(90.));
    assert_eq!(buckets[1].values, [None; 7]);
    assert_eq!(buckets[2].values, [None; 7]);
    assert_eq!(crate::storage::Bucket::average(&buckets, 0), Some(20.));
    assert_eq!(crate::storage::Bucket::average(&buckets, 4), Some(75.5));
    assert_eq!(
        crate::storage::Bucket::extrema(&buckets, 4),
        Some((61., 90.))
    );
    let h = s.hosts.get_mut("a").unwrap();
    h.failures = 0;
    h.metrics.as_mut().unwrap().cpu_percent = Some(80.);
    db.append(&s, 250, 30).unwrap();
    let buckets = crate::storage::history(tmp.path(), "a", 0, 300, 3).unwrap();
    // Two samples in one bucket, one in another: averaging bucket means is wrong.
    assert_eq!(crate::storage::Bucket::average(&buckets, 0), Some(40.));
    assert!(crate::storage::Bucket::average(&[], 0).is_none());
}
