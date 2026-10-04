#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![deny(unsafe_code)]

use makeport::engine::{self, Action, View};
use makeport::model::{unique_id, Draft, ManualPort};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

mod generated {
    // Slint generates unsafe FFI internals. Hand-written application code stays checked.
    #![allow(unsafe_code)]
    slint::include_modules!();
}
use generated::*;

fn parse_manual_port(local: &str, remote: &str, description: &str) -> makeport::Result<ManualPort> {
    let remote = remote
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|p| *p != 0)
        .ok_or("Укажите порт на сервере: от 1 до 65535")?;
    let local = if local.trim().is_empty() {
        remote
    } else {
        local
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or("Локальный порт: от 1 до 65535")?
    };
    let description = if description.trim().is_empty() {
        format!("Порт {remote}")
    } else {
        description.trim().into()
    };
    let port = ManualPort {
        local,
        remote,
        description,
    };
    port.validate()?;
    Ok(port)
}

fn draft(ui: &MainWindow) -> Draft {
    let host = ui.get_edit_host().trim().to_owned();
    Draft {
        id: ui.get_edit_id().to_string(),
        name: if ui.get_edit_name().trim().is_empty() {
            ui.get_edit_path()
                .trim()
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|p| !p.is_empty() && *p != "~")
                .unwrap_or(&host)
                .to_owned()
        } else {
            ui.get_edit_name().to_string()
        },
        host,
        path: ui.get_edit_path().to_string(),
        ports: ui.get_edit_ports().to_string(),
        tasks: ui.get_edit_tasks().to_string(),
        reconnect: ui.get_edit_reconnect(),
    }
}

fn update_tasks(ui: &MainWindow, view: &View) {
    ui.set_tasks(ModelRc::new(VecModel::from(
        view.tasks
            .iter()
            .map(|t| TaskItem {
                name: t.name.clone().into(),
                description: t.description.clone().into(),
                title: t.title.clone().into(),
                state: t.state.clone().into(),
                active: t.active,
                can_run: t.can_run,
                can_stop: t.can_stop,
                unknown: t.unknown,
            })
            .collect::<Vec<_>>(),
    )));
}

fn send(ui: &MainWindow, tx: &SyncSender<Action>, action: Action) {
    if let Err(error) = tx.try_send(action) {
        ui.set_notice(format!("Команда не отправлена: {error}").into());
    }
}

fn edit(ui: &MainWindow, draft: Draft) {
    // Keep one identifier throughout editing: rapid repeated saves are idempotent.
    let is_new = draft.id.is_empty();
    ui.set_edit_is_new(is_new);
    ui.set_edit_id(if is_new { unique_id() } else { draft.id }.into());
    ui.set_edit_name(draft.name.into());
    ui.set_edit_host(draft.host.into());
    ui.set_edit_path(draft.path.into());
    ui.set_edit_ports(draft.ports.into());
    ui.set_edit_tasks(draft.tasks.into());
    ui.set_edit_reconnect(draft.reconnect);
    ui.set_edit_error("".into());
    ui.set_editing(true);
}

fn apply(ui: &MainWindow, view: &View, previous: &View) {
    // Keep models stable while only logs change. This preserves focus/scrolling.
    if view.projects != previous.projects || view.project_states != previous.project_states {
        ui.set_project_titles(ModelRc::new(VecModel::from(
            view.projects
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let label = if view
                        .projects
                        .iter()
                        .filter(|other| other.name == p.name)
                        .count()
                        == 1
                    {
                        p.name.clone()
                    } else {
                        format!("{}:{}", p.host, p.path)
                    };
                    SharedString::from(format!(
                        "{} · {}",
                        label,
                        view.project_states
                            .get(i)
                            .map(String::as_str)
                            .unwrap_or("Не подключён")
                    ))
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_projects(ModelRc::new(VecModel::from(
            view.projects
                .iter()
                .map(|p| ProjectItem {
                    title: p.name.clone().into(),
                    host: p.host.clone().into(),
                })
                .collect::<Vec<_>>(),
        )));
    }
    let selected = view.selected.and_then(|i| view.projects.get(i));
    ui.set_selected(view.selected.map(|i| i as i32).unwrap_or(-1));
    ui.set_project_name(selected.map(|p| p.name.clone()).unwrap_or_default().into());
    ui.set_project_location(
        selected
            .map(|p| format!("{} · {}", p.host, p.path))
            .unwrap_or_default()
            .into(),
    );
    ui.set_connection(view.connection.clone().into());
    ui.set_connected(view.connected);
    ui.set_desired(view.desired);
    ui.set_can_change(view.can_change);
    ui.set_can_add(view.can_add);
    ui.set_can_manage_ports(view.can_manage_ports);
    ui.set_total_running(view.total_running as i32);
    ui.set_can_connect(view.can_connect);
    ui.set_running(view.running as i32);
    ui.set_next_action(view.next_action.clone().into());
    ui.set_discovering(view.discovering);
    ui.set_notice(view.notice.clone().into());
    ui.set_fatal(view.fatal);
    ui.set_quitting(view.quitting);
    if view.ports != previous.ports
        || view.port_descriptions != previous.port_descriptions
        || view.port_states != previous.port_states
        || view.port_manual != previous.port_manual
        || view.selected != previous.selected
    {
        ui.set_ports(ModelRc::new(VecModel::from(
            view.ports
                .iter()
                .enumerate()
                .map(|(i, p)| PortItem {
                    local: i32::from(p.local),
                    state: view
                        .port_states
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| "Не подключён".into())
                        .into(),
                    manual: view.port_manual.get(i).copied().unwrap_or(false),
                    can_open: view.port_states.get(i).is_some_and(|s| s == "Проброшен"),
                    mapping: format!("localhost:{} → сервер:{}", p.local, p.remote).into(),
                    detail: view
                        .port_descriptions
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("Порт {}", p.remote))
                        .into(),
                })
                .collect::<Vec<_>>(),
        )));
    }
    if view.tasks != previous.tasks || view.selected != previous.selected {
        update_tasks(ui, view);
    }
    ui.set_log_name(view.log_name.clone().into());
    if view.log_text != previous.log_text {
        ui.set_log_text(view.log_text.clone().into());
        ui.invoke_scroll_log();
    }
    if view.saved_generation != previous.saved_generation {
        ui.set_editing(false);
        ui.set_adding_port(false);
    }
    if ui.get_adding_port() && view.notice_serial != previous.notice_serial {
        ui.set_port_error(view.notice.clone().into());
    }
    if view.quit_ready {
        let _ = slint::quit_event_loop();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = MainWindow::new()?;
    ui.set_mono_font(
        if cfg!(target_os = "macos") {
            "Menlo"
        } else if cfg!(windows) {
            "Consolas"
        } else {
            "monospace"
        }
        .into(),
    );
    let backend = engine::start();
    let state = Rc::new(RefCell::new(View::default()));

    macro_rules! action0 {
        ($callback:ident, $action:expr) => {{
            let weak = ui.as_weak();
            let tx = backend.actions.clone();
            ui.$callback(move || {
                if let Some(ui) = weak.upgrade() {
                    send(&ui, &tx, $action);
                }
            });
        }};
    }
    macro_rules! action_string {
        ($callback:ident, $variant:path) => {{
            let weak = ui.as_weak();
            let tx = backend.actions.clone();
            ui.$callback(move |value: SharedString| {
                if let Some(ui) = weak.upgrade() {
                    send(&ui, &tx, $variant(value.to_string()));
                }
            });
        }};
    }
    action0!(on_refresh_tasks, Action::Refresh);
    action0!(on_clear_log, Action::ClearLog);
    action0!(on_open_config, Action::OpenConfig);
    action0!(on_dismiss_notice, Action::DismissNotice);
    action0!(on_delete_project, Action::Delete);
    action0!(on_quit, Action::Quit);
    action_string!(on_run_target, Action::Run);
    action_string!(on_stop_target, Action::Stop);
    action_string!(on_reset_target, Action::Reset);
    action_string!(on_select_log, Action::SelectLog);
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_add_port(move |local, remote, description| {
            if let Some(ui) = weak.upgrade() {
                let parsed = parse_manual_port(&local, &remote, &description);
                match parsed {
                    Ok(port) => {
                        ui.set_port_error("".into());
                        send(&ui, &tx, Action::AddPort(port));
                    }
                    Err(error) => ui.set_port_error(error.into()),
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_remove_port(move |local| {
            if let (Some(ui), Ok(local)) = (weak.upgrade(), u16::try_from(local)) {
                send(&ui, &tx, Action::RemovePort(local));
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_retry_port(move |local| {
            if let (Some(ui), Ok(local)) = (weak.upgrade(), u16::try_from(local)) {
                send(&ui, &tx, Action::RetryPort(local));
            }
        });
    }

    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_select_project(move |index| {
            if let Some(ui) = weak.upgrade() {
                if index >= 0 {
                    send(&ui, &tx, Action::Select(index as usize));
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_open_port(move |port| {
            if let (Some(ui), Ok(port)) = (weak.upgrade(), u16::try_from(port)) {
                send(&ui, &tx, Action::OpenPort(port));
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_toggle_connection(move || {
            if let Some(ui) = weak.upgrade() {
                let action = if ui.get_desired() {
                    Action::Disconnect
                } else {
                    Action::Connect
                };
                send(&ui, &tx, action);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_new_project(move || {
            if let Some(ui) = weak.upgrade() {
                send(&ui, &tx, Action::DismissNotice);
                edit(
                    &ui,
                    Draft {
                        reconnect: true,
                        ..Draft::default()
                    },
                );
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        let state = state.clone();
        ui.on_edit_project(move || {
            if let Some(ui) = weak.upgrade() {
                let view = state.borrow();
                if let Some(project) = view.selected.and_then(|i| view.projects.get(i)) {
                    send(&ui, &tx, Action::DismissNotice);
                    edit(&ui, Draft::from(project));
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_save_project(move |connect| {
            if let Some(ui) = weak.upgrade() {
                let draft = draft(&ui);
                match draft.clone().into_project() {
                    Ok(_) => {
                        ui.set_edit_error("".into());
                        send(
                            &ui,
                            &tx,
                            if connect {
                                Action::SaveAndConnect(draft)
                            } else {
                                Action::Save(draft)
                            },
                        );
                    }
                    Err(error) => ui.set_edit_error(error.into()),
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.window().on_close_requested(move || {
            if let Some(ui) = weak.upgrade() {
                if !ui.get_quitting() {
                    if ui.get_total_running() > 0 {
                        ui.set_confirm_kind("quit".into());
                    } else {
                        match tx.try_send(Action::Quit) {
                            Ok(()) => {}
                            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                                return slint::CloseRequestResponse::HideWindow;
                            }
                            Err(error) => {
                                ui.set_notice(format!("Закрытие пока не выполнено: {error}").into())
                            }
                        }
                    }
                }
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let views = backend.views;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
        let Some(ui) = weak.upgrade() else { return; };
        let mut latest = None;
        let mut disconnected = false;
        loop {
            match views.try_recv() {
                Ok(view) => latest = Some(view),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => { disconnected = true; break; }
            }
        }
        if let Some(view) = latest {
            apply(&ui, &view, &state.borrow());
            *state.borrow_mut() = view;
        }
        if disconnected && !ui.get_quitting() {
            ui.set_notice("Рабочий поток завершился неожиданно. Закройте приложение и проверьте задачи на сервере.".into());
            ui.set_can_change(false);
            ui.set_can_add(false);
            ui.set_can_manage_ports(false);
            ui.set_can_connect(false);
            ui.set_running(0);
            ui.set_total_running(0);
            ui.set_fatal(true);
        }
    });
    let result = ui.run();
    timer.stop();
    // Covers an unexpected GUI event-loop exit as well as the normal close flow.
    let _ = backend.actions.send(Action::Quit);
    drop(timer);
    drop(backend.actions);
    let _ = backend.thread.join();
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::Model;

    struct TestPlatform(Rc<MinimalSoftwareWindow>);
    impl slint::platform::Platform for TestPlatform {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn manual_port_form_defaults_and_rejects_invalid_values() {
        let port = parse_manual_port("", "5173", "").unwrap();
        assert_eq!((port.local, port.remote), (5173, 5173));
        assert_eq!(port.description, "Порт 5173");
        assert_eq!(
            parse_manual_port("15432", "5432", " Database ")
                .unwrap()
                .description,
            "Database"
        );
        for (local, remote) in [("0", "5173"), ("", "65536"), ("-1", "80"), ("", "")] {
            assert!(parse_manual_port(local, remote, "Test").is_err());
        }
        assert!(parse_manual_port("80", "80", "two\nlines").is_err());
    }

    #[test]
    fn folder_form_and_config_descriptions_reach_the_ui() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(window))).unwrap();
        let ui = MainWindow::new().unwrap();
        edit(&ui, Draft::default());
        assert!(draft(&ui).into_project().is_err());
        ui.set_edit_host("developer@devbox".into());
        ui.set_edit_path("~/projects/site".into());
        let project = draft(&ui).into_project().unwrap();
        assert_eq!(project.name, "site");
        edit(&ui, Draft::from(&project));
        assert_eq!(draft(&ui).into_project().unwrap(), project);
        let view = View {
            projects: vec![project],
            selected: Some(0),
            connected: true,
            ports: vec![makeport::model::Forward {
                local: 5173,
                remote: 80,
            }],
            port_descriptions: vec!["Сайт клиента".into()],
            tasks: vec![engine::TaskView {
                name: "dev".into(),
                description: "Команда: dev".into(),
                title: "Запустить сайт клиента".into(),
                state: "Работает".into(),
                active: true,
                can_run: false,
                can_stop: true,
                unknown: false,
            }],
            ..View::default()
        };
        apply(&ui, &view, &View::default());
        assert_eq!(ui.get_ports().row_data(0).unwrap().detail, "Сайт клиента");
        assert_eq!(
            ui.get_tasks().row_data(0).unwrap().title,
            "Запустить сайт клиента"
        );
        assert_eq!(ui.get_tasks().row_data(0).unwrap().state, "Работает");
        assert!(!ui.get_show_log());
    }
}
