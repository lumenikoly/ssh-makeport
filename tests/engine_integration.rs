#![cfg(unix)]

use makeport::engine::{self, Action, Backend, View};
use makeport::model::Draft;
use std::fs;
use std::os::unix::fs::PermissionsExt;
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
        panic!("Engine condition timed out. Last view: {last:?}");
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(backend) = self.0.take() {
            let _ = backend.actions.send(Action::Quit);
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline {
                match backend.views.recv_timeout(Duration::from_millis(100)) {
                    Ok(view) if view.quit_ready => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    _ => {}
                }
            }
            drop(backend.views);
            let _ = backend.thread.join();
        }
    }
}

// One test owns environment overrides; other tests never mutate them.
#[test]
fn save_auto_connect_discover_run_stop_and_disconnect() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let project = temp.path().join("project with 'quotes'");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("Makefile"), "ok: ## Success\n\t@echo OK_MARKER\nfail: ## Failure\n\t@exit 7\ndev: ## Long-running task\n\t@python3 -c \"import time; print('DEV_READY', flush=True); time.sleep(60)\"\n").unwrap();
    let fake = temp.path().join("fake-ssh");
    fs::write(&fake, include_str!("fake_ssh.py")).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("MAKEPORT_CONFIG_DIR", &config);
    std::env::set_var("MAKEPORT_SSH", &fake);
    let harness = Harness(Some(engine::start()));
    harness.wait(|v| v.can_change);
    harness.send(Action::Save(Draft {
        name: "Demo".into(),
        host: "fake".into(),
        path: project.display().to_string(),
        tasks: "ok, fail, dev".into(),
        reconnect: false,
        ..Draft::default()
    }));
    harness.wait(|v| v.saved_generation == 1);
    harness.send(Action::Run("ok".into()));
    let view = harness.wait(|v| {
        v.tasks
            .iter()
            .any(|t| t.name == "ok" && t.state == "Готово")
    });
    assert!(view.connected);
    assert!(view.log_text.contains("OK_MARKER"));
    harness.send(Action::Run("fail".into()));
    harness.wait(|v| {
        v.tasks
            .iter()
            .any(|t| t.name == "fail" && t.state == "Ошибка (код 2)")
    });
    harness.send(Action::Run("dev".into()));
    harness.wait(|v| v.log_name == "dev" && v.log_text.contains("DEV_READY"));
    harness.send(Action::Run("dev".into()));
    harness.wait(|v| v.notice.contains("уже запущена"));
    harness.send(Action::Stop("dev".into()));
    harness.wait(|v| {
        v.tasks
            .iter()
            .any(|t| t.name == "dev" && t.state == "Остановлена")
    });
    harness.send(Action::Disconnect);
    harness.wait(|v| !v.desired && v.can_change);
    // The directory file is loaded before creating the tunnel, and its menu is authoritative.
    let reserved = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let local = reserved.local_addr().unwrap().port();
    drop(reserved);
    let settings = format!("[[ports]]\nremote=5173\nlocal={local}\ndescription='Preview site'\n[[tasks]]\ntarget='dev'\ndescription='Start preview'\n");
    fs::write(project.join("makeport.toml"), &settings).unwrap();
    harness.send(Action::Connect);
    let view =
        harness.wait(|v| v.connected && v.tasks.len() == 1 && v.tasks[0].title == "Start preview");
    assert_eq!(view.ports[0].local, local);
    assert_eq!(view.port_descriptions, ["Preview site"]);
    harness.send(Action::Run("dev".into()));
    harness.wait(|v| v.tasks[0].state == "Работает");
    fs::write(
        project.join("makeport.toml"),
        settings.replace("Start preview", "Updated preview"),
    )
    .unwrap();
    harness.send(Action::Refresh);
    let view = harness.wait(|v| v.tasks[0].title == "Updated preview");
    assert_eq!(view.tasks[0].state, "Работает");
    fs::write(project.join("makeport.toml"), "bad = true").unwrap();
    harness.send(Action::Refresh);
    let view = harness.wait(|v| v.notice.contains("Ошибка в makeport.toml"));
    assert!(view.connected);
    harness.send(Action::Disconnect);
    harness.wait(|v| !v.desired && v.can_change);
    harness.send(Action::Connect);
    let view = harness.wait(|v| v.connection == "Ошибка подключения");
    assert!(!view.desired && !view.connected);
    assert!(config.join("config.toml").is_file());
    drop(harness);
    std::env::remove_var("MAKEPORT_CONFIG_DIR");
    std::env::remove_var("MAKEPORT_SSH");
}
