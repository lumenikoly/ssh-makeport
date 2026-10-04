use makeport::engine::{self, Action, Backend, View};
use makeport::model::{Config, Draft, ManualPort};
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
fn manual(local: u16) -> ManualPort {
    ManualPort {
        local,
        remote: 3000,
        description: "Manual preview".into(),
    }
}
fn settings(local: u16, description: &str) -> String {
    format!("[[ports]]\nremote=3000\nlocal={local}\ndescription='{description}'\n[[tasks]]\ntarget='dev'\ndescription='{description} dev'\n[[tasks]]\ntarget='unknown'\ndescription='Unknown task'\n")
}
#[test]
fn simultaneous_projects_live_manual_ports_and_persistent_isolation() {
    let temp = tempfile::tempdir().unwrap();
    let remote = temp.path().join("remote");
    fs::create_dir_all(remote.join("a")).unwrap();
    fs::create_dir_all(remote.join("b")).unwrap();
    let reservations = (0..4)
        .map(|_| TcpListener::bind(("127.0.0.1", 0)).unwrap())
        .collect::<Vec<_>>();
    let ports = reservations
        .iter()
        .map(|p| p.local_addr().unwrap().port())
        .collect::<Vec<_>>();
    drop(reservations);
    fs::write(remote.join("a/makeport.toml"), settings(ports[0], "A")).unwrap();
    fs::write(remote.join("b/makeport.toml"), settings(ports[0], "B")).unwrap();
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
        ..Draft::default()
    }));
    app.wait(|v| v.connected && v.tasks.iter().any(|t| t.title == "A dev"));
    app.send(Action::Run("dev".into()));
    app.wait(|v| v.running == 1 && v.log_text.contains("TASK:/a:dev"));
    app.send(Action::AddPort(manual(ports[2])));
    app.wait(|v| v.ports.len() == 2 && v.port_states[1] == "Проброшен");
    assert!(TcpStream::connect(("127.0.0.1", ports[2])).is_ok());
    app.send(Action::RemovePort(ports[2]));
    let v = app.wait(|v| v.ports.len() == 1);
    assert_eq!(v.running, 1);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if TcpListener::bind(("127.0.0.1", ports[2])).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Removing a live manual port must release its listener"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    fs::write(remote.join("fail-local"), ports[3].to_string()).unwrap();
    app.send(Action::AddPort(manual(ports[3])));
    let v = app.wait(|v| v.ports.len() == 2 && v.port_states[1] == "Ошибка");
    assert!(v.connected);
    assert_eq!(v.running, 1);
    fs::remove_file(remote.join("fail-local")).unwrap();
    app.send(Action::RetryPort(ports[3]));
    app.wait(|v| v.ports.len() == 2 && v.port_states[1] == "Проброшен");

    app.send(Action::AddPort(manual(ports[3])));
    let first = app.wait(|v| v.notice.contains("уже добавлен"));
    app.send(Action::AddPort(manual(ports[3])));
    let repeated = app.wait(|v| v.notice_serial > first.notice_serial);
    assert_eq!(repeated.notice, first.notice);
    assert_eq!(repeated.ports.len(), 2);

    // Saving a second folder is permitted while the first one's task and ports are live.
    app.send(Action::SaveAndConnect(Draft {
        name: "B".into(),
        host: "fake".into(),
        path: "/b".into(),
        ..Draft::default()
    }));
    let v = app.wait(|v| v.selected == Some(1) && v.connection == "Ошибка подключения");
    assert!(v.notice.contains("занят проектом «A»"));
    assert_eq!(v.total_running, 1);
    assert!(TcpStream::connect(("127.0.0.1", ports[3])).is_ok());
    fs::write(remote.join("b/makeport.toml"), settings(ports[1], "B")).unwrap();
    app.send(Action::Connect);
    app.wait(|v| v.selected == Some(1) && v.connected);
    app.send(Action::Run("dev".into()));
    let v = app.wait(|v| v.total_running == 2 && v.log_text.contains("TASK:/b:dev"));
    assert!(!v.log_text.contains("TASK:/a:dev"));
    assert_eq!(v.port_descriptions, ["B"]);
    app.send(Action::AddPort(manual(ports[3])));
    let v = app.wait(|v| v.notice.contains("занят проектом «A»"));
    assert_eq!(v.ports.len(), 1);
    assert!(v.connected);

    app.send(Action::Delete);
    let v = app.wait(|v| v.notice.contains("Сначала отключитесь"));
    assert_eq!(v.projects.len(), 2);
    app.send(Action::Save(Draft {
        name: "Idle folder".into(),
        host: "fake".into(),
        path: "/idle".into(),
        ..Draft::default()
    }));
    let v = app.wait(|v| v.selected == Some(2));
    assert_eq!(v.total_running, 2);
    assert!(v.can_change);
    app.send(Action::Delete);
    let v = app.wait(|v| v.projects.len() == 2 && v.selected == Some(0));
    assert_eq!(v.total_running, 2);

    app.send(Action::Select(0));
    let v = app.wait(|v| v.selected == Some(0));
    assert_eq!(v.total_running, 2);
    assert_eq!(v.running, 1);
    assert!(v.log_text.contains("TASK:/a:dev"));
    assert_eq!(v.port_descriptions, ["A", "Manual preview"]);
    app.send(Action::Stop("dev".into()));
    app.wait(|v| v.running == 0 && v.total_running == 1);
    app.send(Action::Run("unknown".into()));
    app.wait(|v| v.tasks.iter().any(|t| t.name == "unknown" && t.unknown));
    app.send(Action::Select(1));
    let v = app.wait(|v| v.selected == Some(1));
    assert_eq!(v.running, 1);
    assert!(v.tasks.iter().all(|t| !t.unknown));
    app.send(Action::Select(0));
    app.wait(|v| v.selected == Some(0));
    app.send(Action::Disconnect);
    app.wait(|v| !v.desired && v.connection == "Не подключено");
    app.send(Action::Select(1));
    let v = app.wait(|v| v.selected == Some(1));
    assert!(v.connected);
    assert_eq!(v.running, 1);
    // Quit must stop the hidden B task even though A is selected.
    app.send(Action::Select(0));
    app.wait(|v| v.selected == Some(0));
    app.quit();
    assert!(TcpListener::bind(("127.0.0.1", ports[3])).is_ok());
    assert!(TcpListener::bind(("127.0.0.1", ports[1])).is_ok());
    let config: Config =
        toml::from_str(&fs::read_to_string(config_dir.join("config.toml")).unwrap()).unwrap();
    assert_eq!(config.projects.len(), 2);
    assert_eq!(config.projects[0].manual_ports, [manual(ports[3])]);
    assert_eq!(config.unconfirmed.len(), 1);
    assert_eq!(config.unconfirmed[0].project_id, config.projects[0].id);
    let mut restarted = Harness(Some(engine::start()));
    let v = restarted.wait(|v| v.projects.len() == 2);
    assert!(v.tasks.iter().any(|t| t.name == "unknown" && t.unknown));
    assert_eq!(v.ports[0].local, ports[3]);
    restarted.send(Action::Select(1));
    let v = restarted.wait(|v| v.selected == Some(1));
    assert!(v.tasks.iter().all(|t| !t.unknown));
    restarted.quit();
    std::env::remove_var("MAKEPORT_CONFIG_DIR");
    std::env::remove_var("MAKEPORT_SSH");
    std::env::remove_var("MAKEPORT_TEST_REMOTE");
}
