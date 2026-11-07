use makeport::engine::{self, Action, Backend, View};
use makeport::model::{Config, Draft, Port};
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

struct Harness(Option<Backend>);
impl Harness {
    fn send(&self, action: Action) {
        self.0.as_ref().unwrap().actions.send(action).unwrap();
    }
    fn wait(&self, condition: impl Fn(&View) -> bool) -> View {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last = View::default();
        while Instant::now() < deadline {
            if let Ok(view) = self
                .0
                .as_ref()
                .unwrap()
                .views
                .recv_timeout(Duration::from_millis(100))
            {
                if condition(&view) {
                    return view;
                }
                last = view;
            }
        }
        panic!("Timed out. Last view: {last:?}");
    }
    fn quit(&mut self) {
        if let Some(backend) = self.0.take() {
            let _ = backend.actions.send(Action::Quit);
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline {
                match backend.views.recv_timeout(Duration::from_millis(100)) {
                    Ok(view) if view.quit_ready => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    _ => (),
                }
            }
            drop(backend.views);
            backend.thread.join().unwrap();
        }
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.quit();
    }
}
fn manual(local: u16) -> Port {
    Port {
        local,
        remote: 3000,
        description: "Manual preview".into(),
    }
}
#[test]
fn one_config_batches_ports_and_preserves_independent_tasks() {
    let temp = tempfile::tempdir().unwrap();
    let remote = temp.path().join("remote");
    fs::create_dir_all(remote.join("a")).unwrap();
    fs::create_dir_all(remote.join("b")).unwrap();
    // A remote config must never be read, even when present and invalid.
    fs::write(remote.join("a/makeport.toml"), "invalid = [").unwrap();
    let reservations = (0..68)
        .map(|_| TcpListener::bind(("127.0.0.1", 0)).unwrap())
        .collect::<Vec<_>>();
    let ports = reservations
        .iter()
        .map(|p| p.local_addr().unwrap().port())
        .collect::<Vec<_>>();
    drop(reservations);
    let source = temp.path().join("ssh.rs");
    let ssh = temp
        .path()
        .join(if cfg!(windows) { "ssh.exe" } else { "ssh" });
    fs::write(&source, include_str!("fixtures/ssh.rs")).unwrap();
    assert!(std::process::Command::new("rustc")
        .args(["--edition=2021", "--crate-name", "test_ssh"])
        .arg(&source)
        .arg("-o")
        .arg(&ssh)
        .status()
        .unwrap()
        .success());
    let config_dir = temp.path().join("config");
    std::env::set_var("MAKEPORT_CONFIG_DIR", &config_dir);
    std::env::set_var("MAKEPORT_SSH", &ssh);
    std::env::set_var("MAKEPORT_TEST_REMOTE", &remote);
    let mut app = Harness(Some(engine::start()));
    app.wait(|v| v.can_add);
    app.send(Action::SaveAndConnect(Draft {
        name: "A".into(),
        host: "fake".into(),
        path: "/a".into(),
        ports: ports[..64]
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(","),
        tasks: "dev, unknown".into(),
        ..Draft::default()
    }));
    let v = app.wait(|v| v.connected);
    assert_eq!(v.ports.len(), 64);
    for port in &ports[..64] {
        assert!(
            TcpStream::connect(("127.0.0.1", *port)).is_ok(),
            "Port {port} did not listen: {:?}",
            TcpStream::connect(("127.0.0.1", *port))
        );
    }
    assert_eq!(
        fs::read_to_string(remote.join("invocations")).unwrap(),
        "tunnel:64\n"
    );
    app.send(Action::Run("dev".into()));
    app.wait(|v| v.running == 1 && v.log_text.contains("TASK:/a:dev"));
    app.send(Action::AddPorts(vec![manual(ports[65]), manual(ports[67])]));
    let v = app.wait(|v| v.connected && v.ports.len() == 66);
    assert_eq!(v.running, 1);
    assert!(TcpStream::connect(("127.0.0.1", ports[65])).is_ok());
    assert!(TcpStream::connect(("127.0.0.1", ports[67])).is_ok());
    assert_eq!(
        fs::read_to_string(remote.join("invocations")).unwrap(),
        "tunnel:64\ntask:0\ntunnel:66\n"
    );
    // One duplicate rejects the whole edit and leaves the current tunnel running.
    app.send(Action::AddPorts(vec![manual(ports[66]), manual(ports[65])]));
    let v = app.wait(|v| v.notice.contains("уже добавлен"));
    assert_eq!(v.ports.len(), 66);
    assert!(v.connected);
    app.send(Action::RemovePort(ports[65]));
    let v = app.wait(|v| v.connected && v.ports.len() == 65);
    assert_eq!(v.running, 1);
    assert!(TcpListener::bind(("127.0.0.1", ports[65])).is_ok());
    app.send(Action::RemovePort(ports[67]));
    app.wait(|v| v.connected && v.ports.len() == 64);
    // A bind failure fails the entire batch rather than reporting partial success.
    fs::write(remote.join("fail-local"), ports[66].to_string()).unwrap();
    app.send(Action::AddPort(manual(ports[66])));
    let v = app.wait(|v| v.connection == "Ошибка подключения");
    assert_eq!(v.running, 1);
    assert!(!v.connected);
    assert!(v.port_states.iter().all(|s| s == "Ошибка"));
    fs::remove_file(remote.join("fail-local")).unwrap();
    app.send(Action::Connect);
    app.wait(|v| v.connected && v.ports.len() == 65);
    app.send(Action::AddPort(manual(ports[66])));
    app.wait(|v| v.notice.contains("уже добавлен"));
    app.send(Action::SaveAndConnect(Draft {
        name: "B".into(),
        host: "fake".into(),
        path: "/b".into(),
        ports: ports[0].to_string(),
        tasks: "dev".into(),
        ..Draft::default()
    }));
    let v = app.wait(|v| v.selected == Some(1) && v.connection == "Ошибка подключения");
    assert!(v.notice.contains("занят проектом «A»"));
    assert_eq!(v.total_running, 1);
    let mut draft = Draft::from(&v.projects[1]);
    draft.ports = ports[64].to_string();
    app.send(Action::SaveAndConnect(draft));
    app.wait(|v| v.selected == Some(1) && v.connected);
    app.send(Action::Run("dev".into()));
    app.wait(|v| v.total_running == 2 && v.log_text.contains("TASK:/b:dev"));
    app.send(Action::Select(0));
    let v = app.wait(|v| v.selected == Some(0));
    assert_eq!(v.total_running, 2);
    assert!(v.log_text.contains("TASK:/a:dev"));
    app.send(Action::Stop("dev".into()));
    app.wait(|v| v.running == 0 && v.total_running == 1);
    app.send(Action::Run("unknown".into()));
    app.wait(|v| v.tasks.iter().any(|t| t.name == "unknown" && t.unknown));
    app.send(Action::Disconnect);
    app.wait(|v| v.connection == "Не подключено");
    app.send(Action::Refresh);
    app.wait(|v| v.notice.contains("Отключите все подключения"));
    app.quit();
    for port in &ports {
        assert!(TcpListener::bind(("127.0.0.1", *port)).is_ok());
    }
    let path = config_dir.join("config.toml");
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("manual_ports"));
    let mut config: Config = toml::from_str(&text).unwrap();
    assert_eq!(config.projects[0].ports.len(), 65);
    assert_eq!(config.projects[0].ports[64], manual(ports[66]));
    assert_eq!(config.unconfirmed.len(), 1);
    let mut restarted = Harness(Some(engine::start()));
    let v = restarted.wait(|v| v.projects.len() == 2);
    assert!(v.tasks.iter().any(|t| t.name == "unknown" && t.unknown));
    // Reload the single local config while offline, preserving unconfirmed runs.
    config.projects[0].ports.pop();
    config.unconfirmed.clear();
    fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    restarted.send(Action::Refresh);
    let v = restarted.wait(|v| v.ports.len() == 64);
    assert!(v.tasks.iter().any(|t| t.name == "unknown" && t.unknown));
    fs::write(&path, "broken = [").unwrap();
    restarted.send(Action::Refresh);
    let v = restarted.wait(|v| v.notice.contains("Ошибка config.toml"));
    assert_eq!(v.ports.len(), 64);
    assert_eq!(fs::read_to_string(&path).unwrap(), "broken = [");
    restarted.quit();
    std::env::remove_var("MAKEPORT_CONFIG_DIR");
    std::env::remove_var("MAKEPORT_SSH");
    std::env::remove_var("MAKEPORT_TEST_REMOTE");
}
