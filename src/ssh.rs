use crate::{
    config::Host,
    metrics::{self, Metrics},
};
use anyhow::{Context, Result, ensure};
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
pub fn arguments(host: &Host, timeout: u64) -> Vec<String> {
    vec![
        "-T".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={timeout}"),
        "-o".into(),
        "ConnectionAttempts=1".into(),
        host.target().into(),
        "sh -s".into(),
    ]
}
pub async fn poll(host: &Host, timeout: u64) -> Result<(Metrics, u64)> {
    let start = Instant::now();
    let task = async {
        let mut child = Command::new("/usr/bin/ssh")
            .args(arguments(host, timeout))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting /usr/bin/ssh")?;
        let mut stdin = child.stdin.take().context("SSH stdin")?;
        let stdout = child.stdout.take().context("SSH stdout")?;
        let send = async {
            stdin.write_all(include_bytes!("collector.sh")).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<_, anyhow::Error>(())
        };
        let receive = async {
            let mut out = Vec::new();
            stdout.take(1024 * 1024 + 1).read_to_end(&mut out).await?;
            ensure!(out.len() <= 1024 * 1024, "collector output too large");
            Ok::<_, anyhow::Error>(out)
        };
        let (_, out) = tokio::try_join!(send, receive)?;
        let status = child.wait().await?;
        ensure!(
            status.success(),
            "ssh exited with {status}; verify alias, authentication and known_hosts manually"
        );
        Ok((
            metrics::parse(std::str::from_utf8(&out)?)?,
            start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        ))
    };
    tokio::time::timeout(Duration::from_secs(timeout), task)
        .await
        .context("SSH poll timed out")?
}
