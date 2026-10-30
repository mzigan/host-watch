#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[cfg(not(target_os = "linux"))]
compile_error!("host-watch supports Linux only");
mod config;
mod health;
mod metrics;
mod ssh;
mod state;
mod storage;
#[cfg(test)]
mod tests;
mod tui;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use fs2::FileExt;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::Semaphore, task::JoinSet};
#[derive(Parser)]
#[command(version, about = "Agentless Linux host monitoring over OpenSSH")]
struct Cli {
    #[arg(long, global = true, default_value = "/etc/host-watch/config.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Daemon,
    Check,
    Status {
        #[arg(long)]
        verbose: bool,
    },
    Tui,
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
async fn cycle(c: &Config, s: &mut state::State, db: &mut storage::Storage) -> Result<()> {
    let sem = Arc::new(Semaphore::new(c.max_parallel_hosts));
    let mut jobs = JoinSet::new();
    s.hosts
        .retain(|name, _| c.hosts.iter().any(|h| h.enabled && &h.name == name));
    for h in c.hosts.iter().filter(|h| h.enabled).cloned() {
        let sem = sem.clone();
        let timeout = c.ssh_timeout_seconds;
        jobs.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore open");
            let result = ssh::poll(&h, timeout).await;
            (h.name, now(), result)
        });
    }
    while let Some(job) = jobs.join_next().await {
        let (name, timestamp, result) = job.context("poll worker panicked")?;
        let host = s.hosts.entry(name).or_default();
        host.last_poll = Some(timestamp);
        match result {
            Ok((mut m, latency)) => {
                m.derive_cpu(host.metrics.as_ref());
                m.derive_network(host.metrics.as_ref());
                host.previous_success = host.last_success;
                host.last_success = Some(timestamp);
                host.metrics = Some(m);
                host.latency_ms = Some(latency);
                host.failures = 0;
                host.error = None;
            }
            Err(e) => {
                host.failures = host.failures.saturating_add(1);
                host.error = Some(format!("{e:#}"));
            }
        }
        health::evaluate(host, &c.thresholds, timestamp, c.poll_interval_seconds);
    }
    state::write(&c.data_dir, s)?;
    db.append(s, now(), c.history_days)?;
    Ok(())
}
fn counts(s: &state::State) -> [usize; 4] {
    let mut n = [0; 4];
    for h in s.hosts.values() {
        n[match h.health {
            health::Health::Ok => 0,
            health::Health::Warning => 1,
            health::Health::Critical => 2,
            health::Health::Unknown => 3,
        }] += 1;
    }
    n
}
fn display(c: &Config, verbose: bool) -> Result<()> {
    let mut s = state::read(&c.data_dir)?;
    s.hosts
        .retain(|name, _| c.hosts.iter().any(|h| h.enabled && &h.name == name));
    for h in c.hosts.iter().filter(|h| h.enabled) {
        let host = s.hosts.entry(h.name.clone()).or_default();
        if host.last_poll.is_none_or(|t| {
            now().saturating_sub(t)
                > (c.poll_interval_seconds
                    .saturating_mul(2)
                    .saturating_add(c.ssh_timeout_seconds))
                .min(i64::MAX as u64) as i64
        }) {
            if host.health != health::Health::Critical {
                host.health = health::Health::Unknown;
            }
            host.reasons
                .push("poll data stale or not yet collected".into());
        }
    }
    let n = counts(&s);
    println!(
        "HOSTS: {}\nOK: {}\nWARNING: {}\nCRITICAL: {}\nUNKNOWN: {}",
        s.hosts.len(),
        n[0],
        n[1],
        n[2],
        n[3]
    );
    if !s.hosts.is_empty() && n[0] == s.hosts.len() {
        println!("\nAll hosts OK");
    }
    for (name, h) in &s.hosts {
        if verbose || h.health != health::Health::Ok {
            println!(
                "\n{name}: {}{}",
                h.health,
                if h.failures > 0 {
                    " (stale metrics)"
                } else {
                    ""
                }
            );
            for r in &h.reasons {
                println!("  {r}");
            }
            if verbose {
                println!(
                    "  last poll: {:?}; last success: {:?}; failures: {}",
                    h.last_poll, h.last_success, h.failures
                );
                println!("  SSH collection round trip: {:?} ms", h.latency_ms);
                if let Some(m) = &h.metrics {
                    println!("{}", serde_json::to_string_pretty(m)?);
                }
            }
        }
    }
    Ok(())
}
async fn shutdown() -> Result<()> {
    // An atomic signal flag avoids dependence on the signal driver's self-pipe.
    let requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let term = signal_hook::flag::register(signal_hook::consts::SIGTERM, requested.clone())?;
    let interrupt = signal_hook::flag::register(signal_hook::consts::SIGINT, requested.clone())?;
    while !requested.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    signal_hook::low_level::unregister(term);
    signal_hook::low_level::unregister(interrupt);
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let c = Config::read(&cli.config)?;
    match cli.command {
        Commands::Status { verbose } => return display(&c, verbose),
        Commands::Tui => return tui::run(&c),
        _ => {}
    }
    std::fs::create_dir_all(&c.data_dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(c.data_dir.join("writer.lock"))?;
    lock.try_lock_exclusive()
        .context("another daemon/check is already writing this data directory")?;
    let mut s = state::read(&c.data_dir)?;
    let mut db = storage::Storage::open(&c.data_dir)?;
    let stop = shutdown();
    tokio::pin!(stop);
    let mut ticks = tokio::time::interval(Duration::from_secs(c.poll_interval_seconds));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {biased; r=&mut stop=>{r?;break;},_=ticks.tick()=>{}}
        tokio::select! {biased; r=&mut stop=>{r?;break;},r=cycle(&c,&mut s,&mut db)=>r?}
        if matches!(cli.command, Commands::Check) {
            display(&c, false)?;
            break;
        }
    }
    Ok(())
}
