#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![deny(unsafe_code)]

use makeport::engine::{self, Action, View};
use makeport::model::{parse_ports, unique_id, validate_host, validate_path, Draft, Forward};
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

fn port_items(ports: &[Forward]) -> ModelRc<PortItem> {
    ModelRc::new(VecModel::from(
        ports
            .iter()
            .map(|p| PortItem {
                local: i32::from(p.local),
                mapping: format!("localhost:{}", p.local).into(),
                detail: format!("Приложение на сервере: порт {}", p.remote).into(),
            })
            .collect::<Vec<_>>(),
    ))
}

fn update_edit_ports(ui: &MainWindow, ports: Vec<Forward>) {
    ui.set_edit_ports(
        ports
            .iter()
            .map(|p| format!("{}:{}", p.local, p.remote))
            .collect::<Vec<_>>()
            .join(", ")
            .into(),
    );
    ui.set_edit_port_list(port_items(&ports));
}

fn draft(ui: &MainWindow) -> Draft {
    let host = ui.get_edit_host().trim().to_owned();
    let user = ui.get_edit_user().trim().to_owned();
    Draft {
        id: ui.get_edit_id().to_string(),
        name: ui.get_edit_name().to_string(),
        host: if user.is_empty() {
            host
        } else {
            format!("{user}@{host}")
        },
        path: ui.get_edit_path().to_string(),
        ports: ui.get_edit_ports().to_string(),
        tasks: ui.get_edit_tasks().to_string(),
        reconnect: ui.get_edit_reconnect(),
    }
}

fn update_tasks(ui: &MainWindow, view: &View) {
    let query = ui.get_task_filter().trim().to_lowercase();
    ui.set_tasks(ModelRc::new(VecModel::from(
        view.tasks
            .iter()
            .filter(|t| {
                query.is_empty()
                    || t.title.to_lowercase().contains(&query)
                    || t.name.to_lowercase().contains(&query)
            })
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
    let (user, host) = draft.host.split_once('@').unwrap_or(("", &draft.host));
    ui.set_edit_user(user.into());
    ui.set_edit_host(host.into());
    ui.set_edit_path(draft.path.into());
    update_edit_ports(ui, parse_ports(&draft.ports).unwrap_or_default());
    ui.set_edit_tasks(draft.tasks.into());
    ui.set_edit_reconnect(draft.reconnect);
    ui.set_edit_step(0);
    ui.set_edit_advanced(false);
    ui.set_edit_error("".into());
    ui.set_port_remote("".into());
    ui.set_port_local("".into());
    ui.set_editing(true);
}

fn apply(ui: &MainWindow, view: &View, previous: &View) {
    // Keep models stable while only logs change. This preserves focus/scrolling.
    if view.projects != previous.projects {
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
    ui.set_can_connect(view.can_connect);
    ui.set_running(view.running as i32);
    ui.set_next_action(view.next_action.clone().into());
    ui.set_discovering(view.discovering);
    ui.set_notice(view.notice.clone().into());
    ui.set_fatal(view.fatal);
    ui.set_quitting(view.quitting);
    if view.ports != previous.ports {
        ui.set_ports(port_items(&view.ports));
    }
    if view.selected != previous.selected {
        ui.set_task_filter("".into());
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
    }
    if view.quit_ready {
        let _ = slint::quit_event_loop();
    }
}

fn bind_editor(ui: &MainWindow) {
    {
        let weak = ui.as_weak();
        ui.on_next_step(move || {
            if let Some(ui) = weak.upgrade() {
                let d = draft(&ui);
                let result = if ui.get_edit_step() == 0 {
                    if d.name.trim().is_empty() {
                        Err("Назовите проект, чтобы легко найти его в списке.".into())
                    } else {
                        validate_host(&d.host)
                    }
                } else {
                    validate_path(d.path.trim())
                };
                match result {
                    Ok(()) => {
                        ui.set_edit_error("".into());
                        ui.set_edit_step(ui.get_edit_step() + 1);
                    }
                    Err(error) => ui.set_edit_error(error.into()),
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_add_port(move |local: SharedString, remote: SharedString| {
            if let Some(ui) = weak.upgrade() {
                let remote = remote.trim();
                let local = if local.trim().is_empty() {
                    remote
                } else {
                    local.trim()
                };
                let mut existing = ui.get_edit_ports().to_string();
                if !existing.is_empty() {
                    existing.push(',');
                }
                existing.push_str(&format!("{local}:{remote}"));
                match parse_ports(&existing) {
                    Ok(ports) => {
                        update_edit_ports(&ui, ports);
                        ui.set_edit_error("".into());
                        ui.set_port_remote("".into());
                        ui.set_port_local("".into());
                    }
                    Err(error) => ui.set_edit_error(error.into()),
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_remove_port(move |local| {
            if let Some(ui) = weak.upgrade() {
                let ports = parse_ports(&ui.get_edit_ports())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|p| i32::from(p.local) != local)
                    .collect();
                update_edit_ports(&ui, ports);
                ui.set_edit_error("".into());
            }
        });
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
        let state = state.clone();
        ui.on_filter_tasks(move |_| {
            if let Some(ui) = weak.upgrade() {
                update_tasks(&ui, &state.borrow());
            }
        });
    }

    bind_editor(&ui);

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
                if !ui.get_port_remote().trim().is_empty() || !ui.get_port_local().trim().is_empty()
                {
                    ui.set_edit_error(
                        "Нажмите «Добавить доступ» или очистите поля портов перед сохранением."
                            .into(),
                    );
                    return;
                }
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
                    if ui.get_running() > 0 {
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
            ui.set_can_connect(false);
            ui.set_running(0);
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
    fn wizard_validates_steps_and_keeps_port_edits_and_search_consistent() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(window))).unwrap();
        let ui = MainWindow::new().unwrap();
        bind_editor(&ui);
        edit(&ui, Draft::default());
        ui.invoke_next_step();
        assert_eq!(ui.get_edit_step(), 0);
        assert!(ui.get_edit_error().contains("Назовите проект"));
        ui.set_edit_name("Сайт".into());
        ui.set_edit_host("devbox".into());
        ui.set_edit_user("developer".into());
        ui.invoke_next_step();
        assert_eq!(ui.get_edit_step(), 1);
        ui.set_edit_path("relative/path".into());
        ui.invoke_next_step();
        assert_eq!(ui.get_edit_step(), 1);
        assert!(!ui.get_edit_error().is_empty());
        ui.set_edit_path("~/projects/site".into());
        ui.invoke_next_step();
        assert_eq!(ui.get_edit_step(), 2);
        ui.invoke_add_port("".into(), "5173".into());
        ui.invoke_add_port("15432".into(), "5432".into());
        assert_eq!(ui.get_edit_port_list().row_count(), 2);
        ui.invoke_add_port("5173".into(), "80".into());
        assert!(ui.get_edit_error().contains("дважды"));
        assert_eq!(ui.get_edit_port_list().row_count(), 2);
        ui.invoke_add_port("".into(), "65536".into());
        assert_eq!(ui.get_edit_port_list().row_count(), 2);
        ui.invoke_remove_port(5173);
        let project = draft(&ui).into_project().unwrap();
        assert_eq!(project.host, "developer@devbox");
        assert_eq!(
            project.ports,
            vec![Forward {
                local: 15432,
                remote: 5432
            }]
        );
        edit(&ui, Draft::from(&project));
        assert_eq!(draft(&ui).into_project().unwrap(), project);
        let view = View {
            tasks: vec![engine::TaskView {
                name: "test".into(),
                description: "Команда: test".into(),
                title: "Проверить проект".into(),
                state: "Готова".into(),
                active: false,
                can_run: true,
                can_stop: false,
                unknown: false,
            }],
            ..View::default()
        };
        for query in ["ПРОВЕРИТЬ", "test"] {
            ui.set_task_filter(query.into());
            update_tasks(&ui, &view);
            assert_eq!(ui.get_tasks().row_count(), 1);
        }
        ui.set_task_filter("missing".into());
        update_tasks(&ui, &view);
        assert_eq!(ui.get_tasks().row_count(), 0);
    }
}
