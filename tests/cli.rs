use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn daemon_stops_on_term_and_int_without_hosts() {
    for signal in ["-TERM", "-INT"] {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config.toml");
        let data = tmp.path().join("data");
        std::fs::write(
            &config,
            format!(
                "data_dir = {:?}\npoll_interval_seconds = 60\n",
                data.to_str().unwrap()
            ),
        )
        .unwrap();
        let bin = env!("CARGO_BIN_EXE_host-watch");
        let mut daemon = Process(
            Command::new(bin)
                .arg("--config")
                .arg(&config)
                .arg("daemon")
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !data.join("state.json").exists() {
            assert!(
                Instant::now() < deadline,
                "daemon did not write first state"
            );
            assert!(daemon.0.try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            Command::new("/bin/kill")
                .args([signal, &daemon.0.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        loop {
            if let Some(status) = daemon.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "daemon ignored {signal}");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            Command::new(bin)
                .arg("--config")
                .arg(&config)
                .arg("status")
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    }
}
