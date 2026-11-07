#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![deny(unsafe_code)]

use makeport::engine::{self, Action, View};
use makeport::model::{parse_ports, unique_id, Draft, Port};
use slint::winit_030::WinitWindowAccessor;
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

fn show_window(ui: &MainWindow) {
    ui.window().set_minimized(false);
    if let Err(error) = ui.show() {
        ui.set_notice(format!("Не удалось открыть окно: {error}").into());
    }
    ui.window()
        .with_winit_window(|window| window.focus_window());
}

fn finish(ui: &MainWindow, tx: &SyncSender<Action>) {
    match tx.try_send(Action::Quit) {
        Ok(()) => {}
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            let _ = slint::quit_event_loop();
        }
        Err(error) => ui.set_notice(format!("Закрытие пока не выполнено: {error}").into()),
    }
}

fn request_quit(ui: &MainWindow, tx: &SyncSender<Action>) {
    if ui.get_quitting() {
        return;
    }
    if ui.get_total_running() > 0 {
        show_window(ui);
        ui.set_confirm_kind("quit".into());
    } else {
        finish(ui, tx);
    }
}

fn hide_to_tray(ui: &MainWindow) {
    if ui.get_can_hide_to_tray() && !ui.get_quitting() {
        if let Err(error) = ui.hide() {
            ui.set_notice(format!("Не удалось свернуть окно: {error}").into());
        }
    }
}

fn create_tray(ui: &MainWindow, tx: &SyncSender<Action>) -> Result<AppTray, slint::PlatformError> {
    let tray = AppTray::new()?;
    let weak = ui.as_weak();
    tray.on_open_window(move || {
        if let Some(ui) = weak.upgrade() {
            show_window(&ui);
        }
    });
    let weak = ui.as_weak();
    let tx = tx.clone();
    tray.on_exit(move || {
        if let Some(ui) = weak.upgrade() {
            request_quit(&ui, &tx);
        }
    });
    Ok(tray)
}

fn tray_status(view: &View) -> SharedString {
    let connected = view
        .project_states
        .iter()
        .filter(|s| s.starts_with("Подключён"))
        .count();
    format!(
        "Проектов подключено: {connected} · Задач работает: {}",
        view.total_running
    )
    .into()
}

// Linux shells may not expose a tray at all. Check outside the UI thread and
// restore a hidden window if its tray host disappears (for example an extension
// is disabled). Never leave the user with an inaccessible running application.
#[cfg(target_os = "linux")]
fn monitor_tray() -> (
    std::sync::mpsc::Receiver<bool>,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let (status_tx, status_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut connection = None;
        loop {
            if connection.is_none() {
                connection = zbus::blocking::connection::Builder::session()
                    .and_then(|b| b.method_timeout(Duration::from_secs(2)).build())
                    .ok();
            }
            let available = connection.as_ref().is_some_and(|connection| {
                zbus::blocking::Proxy::new(
                    connection,
                    "org.kde.StatusNotifierWatcher",
                    "/StatusNotifierWatcher",
                    "org.kde.StatusNotifierWatcher",
                )
                .and_then(|proxy| proxy.get_property::<bool>("IsStatusNotifierHostRegistered"))
                .unwrap_or(false)
            });
            if !available {
                connection = None;
            }
            if status_tx.send(available).is_err()
                || !matches!(
                    stop_rx.recv_timeout(Duration::from_secs(3)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                )
            {
                break;
            }
        }
    });
    (status_rx, stop_tx, thread)
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
    ui.set_edit_advanced(false);
    ui.set_editing(true);
    ui.invoke_focus_editor();
}

fn refresh_ssh_hosts(ui: &MainWindow) {
    if ui.get_ssh_loading() {
        return;
    }
    ui.set_ssh_loading(true);
    let weak = ui.as_weak();
    if let Err(error) = std::thread::Builder::new()
        .name("ssh-config-discovery".into())
        .spawn(move || {
            let discovery = makeport::ssh_config::discover();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_ssh_hosts(ModelRc::new(VecModel::from(
                    discovery
                        .hosts
                        .into_iter()
                        .map(SharedString::from)
                        .collect::<Vec<_>>(),
                )));
                ui.set_ssh_config_message(discovery.warning.into());
                ui.set_ssh_loading(false);
            });
        })
    {
        ui.set_ssh_loading(false);
        ui.set_ssh_config_message(format!("Не удалось прочитать SSH-настройки: {error}").into());
    }
}

fn apply(ui: &MainWindow, view: &View, previous: &View) {
    if view.selected != previous.selected {
        ui.set_port_input("".into());
        ui.set_port_error("".into());
        ui.set_show_tasks(false);
    }
    if view.saved_generation != previous.saved_generation && view.ports.len() > previous.ports.len()
    {
        ui.set_port_input("".into());
        ui.set_port_error("".into());
    }
    if view.tasks.iter().any(|task| task.unknown)
        && (view.selected != previous.selected || !previous.tasks.iter().any(|task| task.unknown))
    {
        ui.set_show_tasks(true);
    }

    // Keep models stable while only logs change. This preserves focus/scrolling.
    if view.projects != previous.projects || view.project_states != previous.project_states {
        ui.set_projects(ModelRc::new(VecModel::from(
            view.projects
                .iter()
                .enumerate()
                .map(|(i, p)| ProjectItem {
                    title: if view
                        .projects
                        .iter()
                        .filter(|other| other.name == p.name)
                        .count()
                        > 1
                    {
                        format!("{} · {}", p.name, p.path)
                    } else {
                        p.name.clone()
                    }
                    .into(),
                    host: p.host.clone().into(),
                    location: format!("{} · {}", p.host, p.path).into(),
                    state: view
                        .project_states
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| "Не подключён".into())
                        .into(),
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
    ui.set_can_reload(view.can_reload);
    ui.set_can_manage_ports(view.can_manage_ports);
    ui.set_total_running(view.total_running as i32);
    ui.set_can_connect(view.can_connect);
    ui.set_running(view.running as i32);
    ui.set_next_action(view.next_action.clone().into());
    ui.set_notice(view.notice.clone().into());
    ui.set_fatal(view.fatal);
    ui.set_quitting(view.quitting);
    if view.ports != previous.ports
        || view.port_descriptions != previous.port_descriptions
        || view.port_states != previous.port_states
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
                    can_open: view.port_states.get(i).is_some_and(|s| s == "Проброшен"),
                    mapping: format!("localhost:{}", p.local).into(),
                    remote: i32::from(p.remote),
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
    }
    if ui.get_editing() && view.notice_serial != previous.notice_serial {
        ui.set_edit_error(view.notice.clone().into());
    }
    if view.quit_ready {
        let _ = slint::quit_event_loop();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = MainWindow::new()?;
    ui.set_can_hide_to_tray(cfg!(any(target_os = "windows", target_os = "macos")));
    #[cfg(target_os = "linux")]
    let (tray_availability, tray_stop, tray_thread) = monitor_tray();
    ui.window().set_size(slint::LogicalSize::new(1020.0, 740.0));
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
    let tray = Rc::new(RefCell::new(if ui.get_can_hide_to_tray() {
        match create_tray(&ui, &backend.actions) {
            Ok(tray) => Some(tray),
            Err(error) => {
                ui.set_can_hide_to_tray(false);
                ui.set_notice(format!("Трей недоступен: {error}").into());
                None
            }
        }
    } else {
        None
    }));
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
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_quit(move || {
            if let Some(ui) = weak.upgrade() {
                finish(&ui, &tx);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_request_quit(move || {
            if let Some(ui) = weak.upgrade() {
                request_quit(&ui, &tx);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_hide_to_tray(move || {
            if let Some(ui) = weak.upgrade() {
                hide_to_tray(&ui);
            }
        });
    }
    action_string!(on_run_target, Action::Run);
    action_string!(on_stop_target, Action::Stop);
    action_string!(on_reset_target, Action::Reset);
    action_string!(on_select_log, Action::SelectLog);
    {
        let weak = ui.as_weak();
        let tx = backend.actions.clone();
        ui.on_add_ports(move |input| {
            if let Some(ui) = weak.upgrade() {
                match parse_ports(&input) {
                    Ok(ports) if !ports.is_empty() => {
                        ui.set_port_error("".into());
                        send(
                            &ui,
                            &tx,
                            Action::AddPorts(
                                ports
                                    .into_iter()
                                    .map(|port| Port {
                                        local: port.local,
                                        remote: port.remote,
                                        description: String::new(),
                                    })
                                    .collect(),
                            ),
                        );
                    }
                    Ok(_) => ui.set_port_error("Укажите хотя бы один порт".into()),
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
                refresh_ssh_hosts(&ui);
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
                refresh_ssh_hosts(&ui);
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
                if ui.get_can_hide_to_tray() && !ui.get_quitting() {
                    return slint::CloseRequestResponse::HideWindow;
                }
                request_quit(&ui, &tx);
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let tray = tray.clone();
    #[cfg(target_os = "linux")]
    let tx = backend.actions.clone();
    let views = backend.views;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
        let Some(ui) = weak.upgrade() else { return; };
        #[cfg(target_os = "linux")]
        for available in tray_availability.try_iter() {
            if !available && ui.get_can_hide_to_tray() && !ui.window().is_visible() {
                show_window(&ui);
                ui.set_notice("Системный трей недоступен. Окно восстановлено; подключения продолжают работать.".into());
            }
            if available && tray.borrow().is_none() {
                match create_tray(&ui, &tx) {
                    Ok(icon) => *tray.borrow_mut() = Some(icon),
                    Err(error) => ui.set_notice(format!("Трей недоступен: {error}").into()),
                }
            } else if !available {
                tray.borrow_mut().take();
            }
            ui.set_can_hide_to_tray(available && tray.borrow().is_some());
        }
        if ui.get_can_hide_to_tray() && ui.window().is_visible() && ui.window().is_minimized() {
            hide_to_tray(&ui);
        }
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
        if let Some(tray) = tray.borrow().as_ref() {
            tray.set_status(tray_status(&state.borrow()));
            tray.set_quitting(ui.get_quitting());
        }
    });
    let result = ui.run();
    timer.stop();
    #[cfg(target_os = "linux")]
    {
        let _ = tray_stop.send(());
        let _ = tray_thread.join();
    }
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

    struct TestPlatform(Rc<MinimalSoftwareWindow>, Rc<RefCell<String>>);
    impl slint::platform::Platform for TestPlatform {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
        fn set_clipboard_text(&self, text: &str, _: slint::platform::Clipboard) {
            *self.1.borrow_mut() = text.to_owned();
        }
        fn clipboard_text(&self, _: slint::platform::Clipboard) -> Option<String> {
            Some(self.1.borrow().clone())
        }
    }

    #[test]
    fn folder_form_and_config_descriptions_reach_the_ui() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        window.set_size(slint::PhysicalSize::new(880, 660));
        let clipboard = Rc::new(RefCell::new(String::new()));
        slint::platform::set_platform(Box::new(TestPlatform(window.clone(), clipboard.clone())))
            .unwrap();
        let ui = MainWindow::new().unwrap();
        ui.show().unwrap();
        ui.set_ssh_hosts(ModelRc::new(VecModel::from(vec![
            SharedString::from("devbox"),
            SharedString::from("database"),
        ])));
        edit(&ui, Draft::default());
        let capture = |name: &str| {
            let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(880, 660);
            window.draw_if_needed(|renderer| {
                renderer.render(pixels.make_mut_slice(), 880);
            });
            if let Some(directory) = std::env::var_os("MAKEPORT_TEST_SCREENSHOTS") {
                std::fs::create_dir_all(&directory).unwrap();
                let mut image = b"P6\n880 660\n255\n".to_vec();
                image.extend_from_slice(pixels.as_bytes());
                std::fs::write(
                    std::path::Path::new(&directory).join(format!("{name}.ppm")),
                    image,
                )
                .unwrap();
            }
        };
        capture("connection-editor");
        ui.set_edit_ports("15432:5432".into());
        ui.set_edit_name("Custom name".into());
        ui.invoke_show_ssh_hosts();
        capture("ssh-host-picker");
        ui.invoke_use_ssh_host("devbox".into());
        assert_eq!(ui.get_edit_host(), "devbox");
        assert_eq!(ui.get_edit_ports(), "15432:5432");
        assert_eq!(ui.get_edit_name(), "Custom name");
        assert!(ui.get_editing());
        edit(&ui, Draft::default());
        ui.invoke_focus_editor();
        ui.invoke_copy_value("clipboard-check".into());
        assert_eq!(*clipboard.borrow(), "clipboard-check");
        let press = |text: SharedString| {
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
        };
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Control.into(),
            });
        press("м".into()); // Ctrl+V with the Russian keyboard layout.
        assert_eq!(ui.get_edit_host(), "clipboard-check");
        press("ф".into());
        press("с".into());
        assert_eq!(*clipboard.borrow(), "clipboard-check");
        press("ч".into());
        assert!(ui.get_edit_host().is_empty());
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::Control.into(),
            });
        assert!(draft(&ui).into_project().is_err());
        ui.set_edit_host("devbox".into());
        ui.set_edit_ports("3000-3002, 15432:5432".into());
        ui.set_edit_tasks("dev, test".into());
        let forwarding = draft(&ui).into_project().unwrap();
        assert_eq!(forwarding.path, "~");
        assert_eq!(forwarding.ports.len(), 4);
        assert_eq!(forwarding.tasks, ["dev", "test"]);
        ui.set_edit_ports("".into());
        ui.set_edit_tasks("".into());
        ui.set_edit_host("developer@devbox".into());
        ui.set_edit_path("~/projects/site".into());
        let project = draft(&ui).into_project().unwrap();
        assert_eq!(project.name, "site");
        edit(&ui, Draft::from(&project));
        assert!(!ui.get_edit_advanced());
        ui.set_edit_advanced(true);
        capture("advanced-settings");
        ui.set_edit_advanced(false);
        assert_eq!(draft(&ui).into_project().unwrap(), project);
        let view = View {
            projects: vec![project],
            project_states: vec!["Подключён · задач: 1".into()],
            total_running: 1,
            selected: Some(0),
            connected: true,
            desired: true,
            connection: "Подключено".into(),
            running: 1,
            can_add: true,
            can_connect: true,
            can_manage_ports: true,
            port_states: vec!["Проброшен".into()],
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
        assert_eq!(
            tray_status(&view),
            "Проектов подключено: 1 · Задач работает: 1"
        );

        ui.set_editing(false);
        assert!(!ui.get_show_tasks());
        capture("ports-main");
        ui.set_port_input("8000-8002".into());
        let mut rejected = view.clone();
        rejected.notice_serial += 1;
        rejected.notice = "Порт занят".into();
        apply(&ui, &rejected, &view);
        assert_eq!(ui.get_port_input(), "8000-8002");
        let mut saved = rejected.clone();
        saved.saved_generation += 1;
        saved.ports.push(makeport::model::Forward {
            local: 8000,
            remote: 8000,
        });
        apply(&ui, &saved, &rejected);
        assert!(ui.get_port_input().is_empty());
        let mut uncertain = saved.clone();
        uncertain.tasks[0].unknown = true;
        apply(&ui, &uncertain, &saved);
        assert!(ui.get_show_tasks());
        apply(&ui, &view, &uncertain);
        ui.set_can_hide_to_tray(true);
        ui.set_settings_open(true);
        capture("app-settings");
        ui.set_settings_open(false);
        hide_to_tray(&ui);
        assert!(!ui.window().is_visible());
        assert_eq!(ui.get_tasks().row_data(0).unwrap().state, "Работает");
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        ui.set_total_running(1);
        request_quit(&ui, &tx);
        assert!(ui.window().is_visible());
        assert_eq!(ui.get_confirm_kind(), "quit");
        assert!(rx.try_recv().is_err());
        ui.set_confirm_kind("".into());
        ui.set_total_running(0);
        request_quit(&ui, &tx);
        assert!(matches!(rx.try_recv().unwrap(), Action::Quit));
    }
}
