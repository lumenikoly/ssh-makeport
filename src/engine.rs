use crate::log::LogBuffer;
use crate::makefile::{self, Target};
use crate::model::{
    unique_id, validate_target, Config, Draft, Forward, ManualPort, Project, RunRecord, MAX_PORTS,
    MAX_PROJECTS, MAX_RUNNING, MAX_TASKS,
};
use crate::platform;
use crate::process::{IoEvent, Process, Stream};
use crate::project_file::ProjectFile;
use crate::shell::{self, Role};
use crate::store::{config_directory, Store};
use crate::Result;
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Action {
    Select(usize),
    AddPort(ManualPort),
    RemovePort(u16),
    RetryPort(u16),
    Save(Draft),
    SaveAndConnect(Draft),
    Delete,
    Connect,
    Disconnect,
    Refresh,
    Run(String),
    Stop(String),
    Reset(String),
    SelectLog(String),
    ClearLog,
    OpenPort(u16),
    OpenConfig,
    DismissNotice,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Ready,
    Starting,
    Running,
    Stopping,
    Completed(i32),
    Stopped,
    Unknown,
}
impl TaskStatus {
    pub fn active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }
    fn label(&self) -> String {
        match self {
            Self::Ready => "Готова".into(),
            Self::Starting => "Запускается…".into(),
            Self::Running => "Работает".into(),
            Self::Stopping => "Останавливается…".into(),
            Self::Completed(0) => "Готово".into(),
            Self::Completed(code) => format!("Ошибка (код {code})"),
            Self::Stopped => "Остановлена".into(),
            Self::Unknown => "Состояние неизвестно".into(),
        }
    }
}

struct Task {
    name: String,
    description: String,
    status: TaskStatus,
    log: LogBuffer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskView {
    pub name: String,
    pub description: String,
    pub title: String,
    pub state: String,
    pub active: bool,
    pub can_run: bool,
    pub can_stop: bool,
    pub unknown: bool,
}

#[derive(Clone, Debug, Default)]
pub struct View {
    pub projects: Vec<Project>,
    pub project_states: Vec<String>,
    pub can_add: bool,
    pub total_running: usize,
    pub can_manage_ports: bool,
    pub port_states: Vec<String>,
    pub port_manual: Vec<bool>,
    pub selected: Option<usize>,
    pub connection: String,
    pub connected: bool,
    pub desired: bool,
    pub can_change: bool,
    pub can_connect: bool,
    pub running: usize,
    pub tasks: Vec<TaskView>,
    pub ports: Vec<Forward>,
    pub port_descriptions: Vec<String>,
    pub log_name: String,
    pub log_text: String,
    pub notice: String,
    pub notice_serial: u64,
    pub fatal: bool,
    pub saved_generation: u64,
    pub quitting: bool,
    pub quit_ready: bool,
    pub next_action: String,
    pub discovering: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Link {
    Offline,
    Connecting,
    Online,
    Retrying,
    Stopping,
    Failed,
}

fn task_title(name: &str, description: &str) -> String {
    if !description.is_empty() && description != "Добавлена вручную" {
        return description.into();
    }
    match name {
        "dev" | "start" | "serve" => "Запустить приложение",
        "test" => "Проверить проект",
        "build" => "Собрать проект",
        "lint" => "Проверить качество кода",
        "clean" => "Очистить результаты сборки",
        _ => name,
    }
    .into()
}

fn connection_help(error: &str) -> String {
    let help = if error.contains("Permission denied") {
        "Сервер не разрешил вход. Проверьте имя пользователя. Если оно верное, попросите администратора настроить SSH-ключ для этого компьютера."
    } else if error.contains("Host key verification failed")
        || error.contains("REMOTE HOST IDENTIFICATION")
    {
        "Личность сервера не подтверждена. Попросите администратора проверить ключ сервера и добавить его в доверенные на этом компьютере."
    } else if error.contains("Could not resolve hostname")
        || error.contains("Name or service not known")
    {
        "Не удалось найти сервер. Проверьте его адрес и подключение к VPN, если она нужна."
    } else if error.contains("Connection refused") {
        "Сервер отклонил подключение. Уточните у администратора, включён ли SSH и верно ли настроен адрес сервера."
    } else if error.contains("can't cd")
        || error.contains("cannot cd")
        || error.contains("No such file")
    {
        "Папка проекта недоступна. Проверьте путь в настройках и права пользователя на сервере."
    } else if error.contains("make is not available") {
        "На сервере не найден Make. Попросите администратора установить его для запуска задач."
    } else if error.contains("Address already in use") || error.contains("cannot listen") {
        "Адрес на этом компьютере занят другим приложением. Измените local в makeport.toml или освободите этот порт."
    } else {
        "Не удалось подключиться. Проверьте интернет, VPN и доступность сервера, затем повторите подключение."
    };
    format!("{help} Технические подробности — в журнале подключения.")
}

pub struct Backend {
    pub actions: SyncSender<Action>,
    pub views: Receiver<View>,
    pub thread: JoinHandle<()>,
}

pub fn start() -> Backend {
    let (actions, commands) = mpsc::sync_channel(64);
    let (updates, views) = mpsc::sync_channel(2);
    let thread = std::thread::spawn(move || run(commands, updates));
    Backend {
        actions,
        views,
        thread,
    }
}

struct Session {
    project_file: Option<ProjectFile>,
    tasks: Vec<Task>,
    log: LogBuffer,
    log_name: String,
    processes: BTreeMap<u64, Process>,
    link: Link,
    desired: bool,
    recovering: bool,
    retry_at: Option<Instant>,
    retries: u32,
    pending_task: Option<String>,
    notice: String,
    port_errors: BTreeMap<u16, String>,
    notice_serial: u64,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            project_file: None,
            tasks: Vec::new(),
            log: LogBuffer::default(),
            log_name: String::new(),
            processes: BTreeMap::new(),
            link: Link::Offline,
            desired: false,
            recovering: false,
            retry_at: None,
            retries: 0,
            pending_task: None,
            notice: String::new(),
            port_errors: BTreeMap::new(),
            notice_serial: 0,
        }
    }
}
impl Session {
    fn busy(&self) -> bool {
        self.desired
            || !self.processes.is_empty()
            || self.tasks.iter().any(|t| t.status == TaskStatus::Unknown)
    }
    fn running(&self) -> usize {
        self.tasks.iter().filter(|t| t.status.active()).count()
    }
    fn summary(&self) -> String {
        let label = match self.link {
            Link::Online => "Подключён",
            Link::Connecting => "Подключается",
            Link::Retrying => "Восстанавливается",
            Link::Stopping => "Отключается",
            Link::Failed => "Ошибка",
            Link::Offline => "Не подключён",
        };
        if self.tasks.iter().any(|t| t.status == TaskStatus::Unknown) {
            return format!("{label} · нужна проверка");
        }
        if self.running() > 0 {
            format!("{label} · задач: {}", self.running())
        } else {
            label.into()
        }
    }
}
struct Engine {
    session: Session,
    sessions: BTreeMap<String, Session>,
    config: Config,
    store: Option<Store>,
    directory: PathBuf,
    selected: Option<usize>,
    io_tx: SyncSender<IoEvent>,
    next_process: u64,
    closing: Option<Instant>,
    fatal: bool,
    saved_generation: u64,
    dirty: bool,
}
impl std::ops::Deref for Engine {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.session
    }
}
impl std::ops::DerefMut for Engine {
    fn deref_mut(&mut self) -> &mut Session {
        &mut self.session
    }
}

impl Engine {
    fn new(io_tx: SyncSender<IoEvent>) -> Self {
        let directory_result = config_directory();
        let directory = directory_result.as_ref().cloned().unwrap_or_default();
        let loaded = directory_result.and_then(|path| Store::open(&path));
        let (store, config, notice, fatal) = match loaded {
            Ok((store, config)) => (Some(store), config, String::new(), false),
            Err(error) => (None, Config::default(), error, true),
        };
        let selected = config
            .selected
            .as_ref()
            .and_then(|id| config.projects.iter().position(|p| &p.id == id))
            .or_else(|| (!config.projects.is_empty()).then_some(0));
        let mut engine = Self {
            session: Session {
                notice,
                ..Session::default()
            },
            sessions: BTreeMap::new(),
            config,
            store,
            directory,
            selected,
            io_tx,
            next_process: 1,
            closing: None,
            fatal,
            saved_generation: 0,
            dirty: true,
        };
        engine.seed_tasks();
        engine
            .log
            .push("Подключение использует ваши ~/.ssh/config, ключи и SSH-агент.");
        engine
            .log
            .push("Для нового сервера сначала выполните обычный ssh и проверьте отпечаток ключа.");
        engine
    }

    // Runtime state is keyed by stable project ID. Selecting a tab never owns a process.
    fn activate(&mut self, index: usize) -> Result<()> {
        let id = self
            .config
            .projects
            .get(index)
            .ok_or("Проект не найден")?
            .id
            .clone();
        if self.selected == Some(index) {
            return Ok(());
        }
        if let Some(old) = self.project().map(|p| p.id.clone()) {
            let state = std::mem::take(&mut self.session);
            self.sessions.insert(old, state);
        }
        self.selected = Some(index);
        if let Some(state) = self.sessions.remove(&id) {
            self.session = state;
        } else {
            self.session = Session::default();
            self.seed_tasks();
        }
        Ok(())
    }
    fn all_processes_empty(&self) -> bool {
        self.processes.is_empty() && self.sessions.values().all(|s| s.processes.is_empty())
    }
    fn total_running(&self) -> usize {
        self.session.running() + self.sessions.values().map(Session::running).sum::<usize>()
    }
    fn io(&mut self, event: IoEvent) {
        let id = match &event {
            IoEvent::Data(id, ..) | IoEvent::Closed(id, ..) | IoEvent::Error(id, ..) => *id,
        };
        let previous = self.selected;
        if !self.processes.contains_key(&id) {
            let owner = self
                .sessions
                .iter()
                .find(|(_, s)| s.processes.contains_key(&id))
                .map(|(id, _)| id.clone());
            let Some(index) =
                owner.and_then(|id| self.config.projects.iter().position(|p| p.id == id))
            else {
                return;
            };
            if self.activate(index).is_err() {
                return;
            }
        }
        self.io_current(event);
        if let Some(index) = previous {
            let _ = self.activate(index);
        }
    }
    fn tick(&mut self) {
        let previous = self.selected;
        for index in 0..self.config.projects.len() {
            // Unvisited, idle projects need no polling or allocated task/log state.
            let id = &self.config.projects[index].id;
            if self.selected != Some(index) && !self.sessions.contains_key(id) {
                continue;
            }
            if self.activate(index).is_ok() {
                self.tick_current();
            }
        }
        if let Some(index) = previous {
            let _ = self.activate(index);
        }
    }
    fn project(&self) -> Option<&Project> {
        self.selected.and_then(|i| self.config.projects.get(i))
    }
    fn busy(&self) -> bool {
        self.desired
            || !self.processes.is_empty()
            || self.tasks.iter().any(|t| t.status == TaskStatus::Unknown)
    }
    fn can_change(&self) -> bool {
        !self.fatal && self.closing.is_none() && !self.busy()
    }
    fn task_mut(&mut self, name: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.name == name)
    }

    fn seed_tasks(&mut self) {
        self.project_file = None;
        self.tasks.clear();
        let project_id = self.project().map(|p| p.id.clone()).unwrap_or_default();
        let unconfirmed = self
            .config
            .unconfirmed
            .iter()
            .filter(|r| r.project_id == project_id)
            .map(|r| r.target.clone())
            .collect::<Vec<_>>();
        for name in unconfirmed {
            self.add_target(Target {
                name: name.clone(),
                description: "Предыдущий запуск не подтверждён".into(),
            });
            if let Some(task) = self.task_mut(&name) {
                task.status = TaskStatus::Unknown;
                task.log
                    .push("В настройках осталась запись о запуске без подтверждённого завершения.");
                task.log.push(
                    "Проверьте процесс на сервере и нажмите «Проверить» перед повторным запуском.",
                );
            }
        }
        let names = self.project().map(|p| p.tasks.clone()).unwrap_or_default();
        for name in names {
            self.add_target(Target {
                name,
                description: "Добавлена вручную".into(),
            });
        }
    }

    fn add_target(&mut self, target: Target) {
        if let Some(task) = self.task_mut(&target.name) {
            if !target.description.is_empty() {
                task.description = target.description;
            }
        } else if self.tasks.len() < MAX_TASKS {
            self.tasks.push(Task {
                name: target.name,
                description: target.description,
                status: TaskStatus::Ready,
                log: LogBuffer::default(),
            });
        }
    }

    fn reconcile_targets(&mut self, targets: Vec<Target>) {
        let manual = self.project().map(|p| p.tasks.clone()).unwrap_or_default();
        // Keep ongoing or unconfirmed work visible even when its rule disappeared.
        self.tasks.retain(|t| {
            t.status.active()
                || t.status == TaskStatus::Unknown
                || manual.contains(&t.name)
                || targets.iter().any(|new| new.name == t.name)
        });
        for target in targets {
            self.add_target(target);
        }
        if !self.tasks.iter().any(|t| t.name == self.log_name) {
            self.log_name.clear();
        }
    }

    fn notice(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.log.push(format!("[!] {message}"));
        self.notice = message;
        self.notice_serial += 1;
        self.dirty = true;
    }

    fn commit(&mut self, config: Config) -> Result<()> {
        self.store
            .as_ref()
            .ok_or("Настройки недоступны")?
            .save(&config)?;
        self.config = config;
        Ok(())
    }

    // Persist the reservation BEFORE spawning SSH. A crash at any later point
    // leaves a conservative record that blocks duplicate launches on restart.
    fn reserve_run(&mut self, target: &str) -> Result<()> {
        let project_id = self.project().ok_or("Проект не выбран")?.id.clone();
        if self
            .config
            .unconfirmed
            .iter()
            .any(|r| r.project_id == project_id && r.target == target)
        {
            return Err("Предыдущий запуск этой задачи ещё не подтверждён".into());
        }
        let mut config = self.config.clone();
        config.unconfirmed.push(RunRecord {
            project_id,
            target: target.into(),
        });
        self.commit(config)
    }

    fn clear_run(&mut self, target: &str) -> Result<()> {
        let project_id = self.project().ok_or("Проект не выбран")?.id.clone();
        let mut config = self.config.clone();
        config
            .unconfirmed
            .retain(|r| !(r.project_id == project_id && r.target == target));
        self.commit(config)
    }

    fn action(&mut self, action: Action) -> Result<()> {
        self.dirty = true;
        if self.closing.is_some() && !matches!(&action, Action::Quit) {
            return Ok(());
        }
        match action {
            Action::SaveAndConnect(draft) => {
                self.action(Action::Save(draft))?;
                self.connect()?;
            }
            Action::Quit => {
                self.closing.get_or_insert_with(Instant::now);
                let previous = self.selected;
                for index in 0..self.config.projects.len() {
                    self.activate(index)?;
                    self.disconnect();
                }
                if let Some(index) = previous {
                    self.activate(index)?;
                }
            }
            Action::AddPort(port) => self.add_port(port)?,
            Action::RemovePort(local) => self.remove_port(local)?,
            Action::RetryPort(local) => {
                let port = self
                    .project()
                    .and_then(|p| p.manual_ports.iter().find(|p| p.local == local))
                    .ok_or("Ручной порт не найден")?
                    .forward();
                if self.link != Link::Online {
                    return Err("Сначала подключитесь к проекту".into());
                }
                self.notice.clear();
                if let Err(error) = self.start_port(port) {
                    self.port_errors.insert(local, error.clone());
                    return Err(error);
                }
            }
            Action::DismissNotice => {
                if !self.fatal {
                    self.notice.clear();
                }
            }
            Action::OpenConfig => platform::open_directory(&self.directory)?,
            Action::SelectLog(name) => {
                if name.is_empty() || self.tasks.iter().any(|t| t.name == name) {
                    self.log_name = name;
                }
            }
            Action::ClearLog => {
                let name = self.log_name.clone();
                if name.is_empty() {
                    self.log.clear();
                } else if let Some(task) = self.task_mut(&name) {
                    task.log.clear();
                }
            }
            Action::Select(index) => {
                if self.fatal {
                    return Err("Настройки недоступны".into());
                }
                let project = self.config.projects.get(index).ok_or("Проект не найден")?;
                let mut config = self.config.clone();
                config.selected = Some(project.id.clone());
                self.commit(config)?;
                self.activate(index)?;
            }
            Action::Save(draft) => {
                if self.fatal {
                    return Err("Настройки недоступны".into());
                }
                let mut project = draft.into_project()?;
                let mut config = self.config.clone();
                let index =
                    if let Some(index) = config.projects.iter().position(|p| p.id == project.id) {
                        let busy = if self.selected == Some(index) {
                            self.busy()
                        } else {
                            self.sessions.get(&project.id).is_some_and(Session::busy)
                        };
                        if busy {
                            return Err("Нельзя менять настройки работающего проекта".into());
                        }
                        project.manual_ports = config.projects[index].manual_ports.clone();
                        project.validate()?;
                        config.projects[index] = project.clone();
                        index
                    } else {
                        if config.projects.len() >= MAX_PROJECTS {
                            return Err("Допускается не более 64 проектов".into());
                        }
                        config.projects.push(project.clone());
                        config.projects.len() - 1
                    };
                config.selected = Some(project.id);
                self.commit(config)?;
                self.activate(index)?;
                self.seed_tasks();
                self.log_name.clear();
                self.notice.clear();
                self.saved_generation += 1;
            }
            Action::Delete => {
                if !self.can_change() {
                    return Err("Сначала отключитесь от проекта".into());
                }
                let index = self.selected.ok_or("Проект не выбран")?;
                let mut config = self.config.clone();
                let deleted = config.projects.remove(index).id;
                config.selected = config.projects.first().map(|p| p.id.clone());
                self.commit(config)?;
                self.sessions.remove(&deleted);
                self.session = Session::default();
                self.selected = None;
                if !self.config.projects.is_empty() {
                    self.activate(0)?;
                }
                self.saved_generation += 1;
            }
            Action::Connect => self.connect()?,
            Action::Disconnect => self.disconnect(),
            Action::Refresh => self.refresh()?,
            Action::Run(target) => self.run_target(target)?,
            Action::Stop(target) => self.stop_target(&target),
            Action::Reset(target) => {
                if self
                    .tasks
                    .iter()
                    .any(|t| t.name == target && t.status == TaskStatus::Unknown)
                {
                    self.clear_run(&target)?;
                }
                if let Some(task) = self.task_mut(&target) {
                    if task.status == TaskStatus::Unknown {
                        task.status = TaskStatus::Ready;
                        task.log.push("Пользователь подтвердил проверку удалённого процесса. Повторный запуск разрешён.");
                    }
                }
            }
            Action::OpenPort(port) => {
                if self.port_state(port) != "Проброшен" {
                    return Err("Этот туннель не подключён".into());
                }
                platform::open_port(port)?;
            }
        }
        Ok(())
    }

    fn configured_ports(&self) -> Vec<Forward> {
        self.project_file
            .as_ref()
            .map(|f| f.forwards())
            .unwrap_or_else(|| self.project().map(|p| p.ports.clone()).unwrap_or_default())
    }

    fn effective_ports(&self) -> Vec<Forward> {
        let mut ports = self.configured_ports();
        if let Some(project) = self.project() {
            ports.extend(project.manual_ports.iter().map(ManualPort::forward));
        }
        ports
    }
    fn port_descriptions(&self) -> Vec<String> {
        let mut descriptions = self
            .configured_ports()
            .iter()
            .enumerate()
            .map(|(index, port)| {
                self.project_file
                    .as_ref()
                    .and_then(|f| f.ports.get(index))
                    .map(|p| p.description.clone())
                    .unwrap_or_else(|| format!("Порт {}", port.remote))
            })
            .collect::<Vec<_>>();
        if let Some(project) = self.project() {
            descriptions.extend(project.manual_ports.iter().map(|p| p.description.clone()));
        }
        descriptions
    }
    fn port_state(&self, local: u16) -> String {
        if self.configured_ports().iter().any(|p| p.local == local) {
            return match self.link {
                Link::Online => "Проброшен",
                Link::Failed => "Ошибка",
                Link::Connecting | Link::Retrying => "Подключается…",
                Link::Stopping => "Отключается…",
                Link::Offline => "Не подключён",
            }
            .into();
        }
        if let Some(process) = self
            .processes
            .values()
            .find(|p| matches!(&p.role, Role::Port(port) if port.local == local))
        {
            return if process.stopping_at.is_some() {
                "Отключается…"
            } else if process.ready {
                "Проброшен"
            } else {
                "Подключается…"
            }
            .into();
        }
        if self.port_errors.contains_key(&local) {
            "Ошибка"
        } else if self.desired {
            "Подключается…"
        } else {
            "Не подключён"
        }
        .into()
    }
    fn check_other_port(&self, local: u16) -> Result<()> {
        for (id, state) in &self.sessions {
            let Some(project) = self.config.projects.iter().find(|p| &p.id == id) else {
                continue;
            };
            let configured = state
                .project_file
                .as_ref()
                .map(|f| f.forwards())
                .unwrap_or_else(|| project.ports.clone());
            let reserved = state.desired
                && (configured.iter().any(|p| p.local == local)
                    || project.manual_ports.iter().any(|p| p.local == local));
            let closing = state
                .processes
                .values()
                .any(|p| matches!(&p.role, Role::Port(port) if port.local == local));
            if reserved || closing {
                return Err(format!("Локальный порт {local} занят проектом «{}» ({}). Выберите другой локальный порт.", project.name, project.path));
            }
        }
        Ok(())
    }
    fn add_port(&mut self, port: ManualPort) -> Result<()> {
        port.validate()?;
        if self.fatal
            || self.closing.is_some()
            || matches!(
                self.link,
                Link::Connecting | Link::Retrying | Link::Stopping
            )
        {
            return Err("Дождитесь завершения подключения".into());
        }
        if self.effective_ports().len() >= MAX_PORTS {
            return Err("В одном проекте допускается не более 32 портов".into());
        }
        if self.effective_ports().iter().any(|p| p.local == port.local) {
            return Err(format!(
                "Локальный порт {} уже добавлен в этот проект",
                port.local
            ));
        }
        if self
            .processes
            .values()
            .any(|p| matches!(&p.role, Role::Port(p) if p.local == port.local))
        {
            return Err("Предыдущий туннель ещё закрывается".into());
        }
        self.check_other_port(port.local)?;
        let reservation = TcpListener::bind(("127.0.0.1", port.local))
            .map_err(|e| format!("Локальный порт {} занят или недоступен: {e}", port.local))?;
        drop(reservation);
        let index = self.selected.ok_or("Сначала откройте папку проекта")?;
        let forward = port.forward();
        let mut config = self.config.clone();
        config.projects[index].manual_ports.push(port);
        self.commit(config)?;
        self.saved_generation += 1;
        self.notice.clear();
        if self.link == Link::Online {
            if let Err(error) = self.start_port(forward.clone()) {
                self.port_errors.insert(forward.local, error.clone());
                self.notice(error);
            }
        }
        Ok(())
    }
    fn remove_port(&mut self, local: u16) -> Result<()> {
        let index = self.selected.ok_or("Проект не выбран")?;
        let mut config = self.config.clone();
        if !config.projects[index]
            .manual_ports
            .iter()
            .any(|p| p.local == local)
        {
            return Err("Этот порт задан в makeport.toml. Измените файл проекта.".into());
        }
        config.projects[index]
            .manual_ports
            .retain(|p| p.local != local);
        self.commit(config)?;
        self.port_errors.remove(&local);
        for process in self.processes.values_mut() {
            if matches!(&process.role, Role::Port(port) if port.local == local) {
                let _ = process.request_stop();
            }
        }
        Ok(())
    }
    fn start_port(&mut self, port: Forward) -> Result<()> {
        if self
            .processes
            .values()
            .any(|p| matches!(&p.role, Role::Port(p) if p.local == port.local))
        {
            return Ok(());
        }
        self.check_other_port(port.local)?;
        let reservation = TcpListener::bind(("127.0.0.1", port.local))
            .map_err(|e| format!("Локальный порт {} занят или недоступен: {e}", port.local))?;
        drop(reservation);
        self.port_errors.remove(&port.local);
        self.spawn(Role::Port(port))?;
        Ok(())
    }

    fn spawn(&mut self, role: Role) -> Result<u64> {
        if self.fatal {
            return Err("Сначала исправьте ошибку настроек".into());
        }
        let mut project = self
            .project()
            .cloned()
            .ok_or("Добавьте и выберите проект")?;
        project.ports = self.configured_ports();
        let executable = platform::ssh_executable()?;
        let token = unique_id();
        let args = shell::arguments(&project, &role, &token)?;
        let id = self.next_process;
        self.next_process += 1;
        let process = Process::spawn(id, role, token, &executable, &args, self.io_tx.clone())?;
        self.processes.insert(id, process);
        Ok(id)
    }

    fn begin_tunnel(&mut self) -> Result<()> {
        self.project().ok_or("Проект не выбран")?;
        // An early diagnostic only. ssh's ExitOnForwardFailure is authoritative:
        // another process can take a port between this check and SSH binding it.
        let mut reservations = Vec::new();
        for port in &self.configured_ports() {
            if self
                .project()
                .is_some_and(|p| p.manual_ports.iter().any(|m| m.local == port.local))
            {
                return Err(format!(
                    "Порт {} указан в makeport.toml и добавлен вручную. Удалите ручной порт.",
                    port.local
                ));
            }
            self.check_other_port(port.local)?;
            reservations.push(TcpListener::bind(("127.0.0.1", port.local)).map_err(|e| {
                format!(
                    "Порт {} недоступен: {e}. Измените local в makeport.toml или освободите этот порт.",
                    port.local
                )
            })?);
        }
        drop(reservations);
        self.spawn(Role::Tunnel)?;
        self.link = Link::Connecting;
        self.log.push("[SSH] Подключение…");
        Ok(())
    }

    fn connect(&mut self) -> Result<()> {
        if self.desired {
            return Ok(());
        }
        if self.processes.values().any(|p| p.role == Role::Tunnel) {
            return Err("Предыдущее подключение ещё закрывается".into());
        }
        self.notice.clear();
        self.desired = true;
        self.recovering = false;
        self.retries = 0;
        self.retry_at = None;
        self.link = Link::Connecting;
        if let Err(error) = self.spawn(Role::Config) {
            self.desired = false;
            self.link = Link::Failed;
            return Err(error);
        }
        Ok(())
    }

    fn refresh(&mut self) -> Result<()> {
        if self.link != Link::Online {
            return Err("Сначала подключитесь к серверу".into());
        }
        if self
            .processes
            .values()
            .any(|p| matches!(p.role, Role::Config | Role::Discover))
        {
            return Ok(());
        }
        self.spawn(Role::Config)?;
        Ok(())
    }

    fn run_target(&mut self, target: String) -> Result<()> {
        let target = target.trim().to_owned();
        validate_target(&target)?;
        if self
            .tasks
            .iter()
            .any(|t| t.name == target && (t.status.active() || t.status == TaskStatus::Unknown))
        {
            return Err("Задача уже запущена или её завершение ещё не подтверждено".into());
        }
        if self.tasks.len() >= MAX_TASKS && !self.tasks.iter().any(|t| t.name == target) {
            return Err("Достигнут предел 128 задач в проекте".into());
        }
        if self.link != Link::Online {
            if self.pending_task.is_some() {
                return Err("Одна задача уже ожидает подключения".into());
            }
            self.connect()?;
            self.pending_task = Some(target);
            return Ok(());
        }
        if self
            .processes
            .values()
            .chain(self.sessions.values().flat_map(|s| s.processes.values()))
            .filter(|p| matches!(p.role, Role::Task(_)))
            .count()
            >= MAX_RUNNING
        {
            return Err("Одновременно можно выполнять не более 8 задач".into());
        }
        self.add_target(Target {
            name: target.clone(),
            description: String::new(),
        });
        self.reserve_run(&target)?;
        if let Err(error) = self.spawn(Role::Task(target.clone())) {
            if let Err(save_error) = self.clear_run(&target) {
                if let Some(task) = self.task_mut(&target) {
                    task.status = TaskStatus::Unknown;
                }
                return Err(format!(
                    "{error}. Не удалось снять запись запуска: {save_error}"
                ));
            }
            return Err(error);
        }
        if let Some(task) = self.task_mut(&target) {
            task.status = TaskStatus::Starting;
            task.log.clear();
            task.log.push(format!("$ make {target}"));
        }
        self.log_name = target;
        Ok(())
    }

    fn stop_target(&mut self, target: &str) {
        let mut failure = None;
        for process in self.processes.values_mut() {
            if matches!(&process.role, Role::Task(name) if name == target) {
                if let Err(error) = process.request_stop() {
                    failure = Some(error);
                }
            }
        }
        if let Some(task) = self.task_mut(target) {
            if task.status.active() {
                task.status = TaskStatus::Stopping;
                task.log
                    .push("[MakePort] Отправлен Ctrl+C. Ожидание подтверждения завершения…");
                if let Some(error) = failure {
                    task.log.push(error);
                }
            }
        }
    }

    fn disconnect(&mut self) {
        self.desired = false;
        self.retry_at = None;
        self.pending_task = None;
        self.link = if self.processes.is_empty() {
            Link::Offline
        } else {
            Link::Stopping
        };
        let names = self
            .tasks
            .iter()
            .filter(|t| t.status.active())
            .map(|t| t.name.clone())
            .collect::<Vec<_>>();
        for name in names {
            self.stop_target(&name);
        }
        for process in self.processes.values_mut() {
            match process.role {
                Role::Config | Role::Discover => process.terminate(),
                Role::Tunnel | Role::Port(_) => {
                    let _ = process.request_stop();
                }
                Role::Task(_) => {}
            }
        }
        self.dirty = true;
    }

    fn io_current(&mut self, event: IoEvent) {
        let (id, stream, lines) = match event {
            IoEvent::Data(id, stream, bytes) => {
                let Some(process) = self.processes.get_mut(&id) else {
                    return;
                };
                if matches!(process.role, Role::Config | Role::Discover)
                    && matches!(stream, Stream::Stdout)
                {
                    let remaining = (262_145_usize).saturating_sub(process.capture.len());
                    process
                        .capture
                        .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
                    process.capture_overflow |=
                        process.capture.len() > 262_144 || bytes.len() > remaining;
                    return;
                }
                (id, stream, process.decoders[stream.index()].feed(&bytes))
            }
            IoEvent::Closed(id, stream) => {
                let Some(process) = self.processes.get_mut(&id) else {
                    return;
                };
                process.close_stream(stream);
                (
                    id,
                    stream,
                    process.decoders[stream.index()]
                        .finish()
                        .into_iter()
                        .collect(),
                )
            }
            IoEvent::Error(id, stream, message) => {
                if let Some(process) = self.processes.get_mut(&id) {
                    process.error = Some(message.clone());
                }
                (id, stream, vec![message])
            }
        };
        for line in lines {
            self.line(id, stream, line);
        }
    }

    fn line(&mut self, id: u64, stream: Stream, raw: String) {
        let Some(process) = self.processes.get_mut(&id) else {
            return;
        };
        let role = process.role.clone();
        if matches!(stream, Stream::Stdout) {
            if let Role::Port(port) = &role {
                if raw == shell::ready_marker(&process.token) {
                    process.ready = true;
                    self.port_errors.remove(&port.local);
                    self.dirty = true;
                    return;
                }
            }
            if role == Role::Tunnel && raw == shell::ready_marker(&process.token) {
                process.ready = true;
                self.link = Link::Online;
                self.retries = 0;
                self.recovering = false;
                self.log.push("[SSH] Подключено. Настроенные туннели подняты; доступность приложения проверяется отдельно.");
                self.dirty = true;
                if self.desired {
                    let ports = self
                        .project()
                        .map(|p| {
                            p.manual_ports
                                .iter()
                                .map(ManualPort::forward)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    for port in ports {
                        if let Err(error) = self.start_port(port.clone()) {
                            self.port_errors.insert(port.local, error.clone());
                            self.notice(error);
                        }
                    }
                    if self.project_file.is_none() {
                        if let Err(error) = self.spawn(Role::Discover) {
                            self.notice(error);
                        }
                    }
                    if let Some(target) = self.pending_task.take() {
                        if let Err(error) = self.run_target(target) {
                            self.notice(error);
                        }
                    }
                } else {
                    self.disconnect();
                }
                return;
            }
            if let Role::Task(name) = &role {
                if raw == shell::started_marker(&process.token) {
                    process.ready = true;
                    let stopping = process.stopping_at.is_some();
                    if let Some(task) = self.task_mut(name) {
                        task.status = if stopping {
                            TaskStatus::Stopping
                        } else {
                            TaskStatus::Running
                        };
                    }
                    self.dirty = true;
                    return;
                }
                if let Some(code) = raw
                    .strip_prefix(shell::exit_marker(&process.token).as_str())
                    .and_then(|s| s.parse::<i32>().ok())
                {
                    if (0..=255).contains(&code) {
                        process.remote_exit = Some(code);
                        return;
                    }
                }
            }
        }
        let clean = process.filters[stream.index()].clean(&raw);
        if matches!(stream, Stream::Stderr) && !clean.is_empty() {
            process.error = Some(clean.clone());
        }
        match role {
            Role::Task(name) => {
                if let Some(task) = self.task_mut(&name) {
                    task.log.push(clean);
                }
            }
            _ => self.log.push(clean),
        }
        self.dirty = true;
    }

    fn finished(&mut self, mut process: Process) {
        // Consume final unterminated output before calculating task state.
        for stream in [Stream::Stdout, Stream::Stderr] {
            if let Some(line) = process.decoders[stream.index()].finish() {
                let clean = process.filters[stream.index()].clean(&line);
                match &process.role {
                    Role::Task(name) => {
                        if let Some(task) = self.task_mut(name) {
                            task.log.push(clean);
                        }
                    }
                    _ => self.log.push(clean),
                }
            }
        }
        self.dirty = true;
        match &process.role {
            Role::Tunnel => {
                if !self.desired {
                    return;
                }
                let error = process
                    .error
                    .clone()
                    .unwrap_or_else(|| "SSH-соединение закрыто".into());
                self.log.push(format!("[SSH] {error}"));
                let permanent = [
                    "Permission denied",
                    "Host key verification failed",
                    "REMOTE HOST IDENTIFICATION",
                    "Bad configuration",
                    "Address already in use",
                    "cannot listen",
                    "not available",
                    "can't cd",
                    "cannot cd",
                ]
                .iter()
                .any(|pattern| error.contains(pattern));
                let retry = self.project().is_some_and(|p| p.reconnect)
                    && (process.ready || self.recovering)
                    && self.retries < 5
                    && !permanent;
                if retry {
                    self.recovering = true;
                    self.retries += 1;
                    let seconds = (1_u64 << self.retries).min(30);
                    self.retry_at = Some(Instant::now() + Duration::from_secs(seconds));
                    self.link = Link::Retrying;
                    self.session.log.push(format!(
                        "[SSH] Повтор {}/5 через {seconds} с. Задачи не перезапускаются.",
                        self.retries
                    ));
                } else {
                    self.link = Link::Failed;
                    self.desired = false;
                    self.pending_task = None;
                    for process in self.processes.values_mut() {
                        if matches!(process.role, Role::Port(_)) {
                            let _ = process.request_stop();
                        }
                    }
                    self.notice(connection_help(&error));
                }
            }
            Role::Port(port) => {
                if self.desired
                    && self
                        .project()
                        .is_some_and(|p| p.manual_ports.iter().any(|m| m.local == port.local))
                {
                    let error = process
                        .error
                        .clone()
                        .unwrap_or_else(|| "SSH-туннель закрылся".into());
                    self.port_errors.insert(port.local, error.clone());
                    self.notice(format!(
                        "Порт {}: {error}. Нажмите «Повторить».",
                        port.local
                    ));
                }
            }
            Role::Config => {
                if !self.desired {
                    return;
                }
                let result = if process.capture_overflow {
                    Err("makeport.toml слишком большой".into())
                } else if process.status.is_some_and(|s| s.success()) {
                    std::str::from_utf8(&process.capture)
                        .map_err(|_| "makeport.toml должен быть в UTF-8".to_string())
                        .and_then(ProjectFile::parse)
                        .map(Some)
                } else if process.status.and_then(|s| s.code()) == Some(3) {
                    Ok(None)
                } else {
                    Err(connection_help(
                        process
                            .error
                            .as_deref()
                            .unwrap_or("Не удалось прочитать makeport.toml"),
                    ))
                };
                match result {
                    Ok(file) => {
                        let ports = file.as_ref().map(|f| f.forwards()).unwrap_or_else(|| {
                            self.project().map(|p| p.ports.clone()).unwrap_or_default()
                        });
                        if self.link == Link::Online && ports != self.configured_ports() {
                            self.notice("Порты в makeport.toml изменились. Отключитесь и подключитесь снова, чтобы применить их.");
                            return;
                        }
                        if let Some(file) = &file {
                            // Configured actions form the menu; preserve only ongoing or uncertain runs.
                            self.tasks.retain(|t| {
                                t.status.active()
                                    || t.status == TaskStatus::Unknown
                                    || file.tasks.iter().any(|a| a.target == t.name)
                            });
                            for task in &file.tasks {
                                self.add_target(Target {
                                    name: task.target.clone(),
                                    description: task.description.clone(),
                                });
                            }
                            self.log.push("[Проект] Загружен makeport.toml");
                        }
                        self.project_file = file;
                        if !self.tasks.iter().any(|t| t.name == self.log_name) {
                            self.log_name.clear();
                        }
                        if self.link != Link::Online {
                            if let Err(error) = self.begin_tunnel() {
                                self.desired = false;
                                self.link = Link::Failed;
                                self.notice(error);
                            }
                        } else if self.project_file.is_none() {
                            if let Err(error) = self.spawn(Role::Discover) {
                                self.notice(error);
                            }
                        }
                    }
                    Err(error) => {
                        if self.link != Link::Online {
                            self.desired = false;
                            self.link = Link::Failed;
                            self.pending_task = None;
                        }
                        self.notice(error);
                    }
                }
            }
            Role::Discover => {
                if !self.desired {
                    return;
                }
                if process.capture_overflow {
                    self.notice(
                        "Makefile больше 256 КиБ. Укажите нужные действия в makeport.toml.",
                    );
                } else if process.status.is_some_and(|s| s.success()) {
                    let targets = makefile::discover(&String::from_utf8_lossy(&process.capture));
                    let count = targets.len();
                    self.reconcile_targets(targets);
                    self.log.push(format!("[Makefile] Найдено целей: {count}. Комментарии ## имеют приоритет над .PHONY."));
                } else {
                    self.notice("В папке нет доступного Makefile. Добавьте makeport.toml с нужными действиями; подробности — в журнале подключения.");
                }
            }
            Role::Task(name) => {
                let saved = if process.remote_exit.is_some() {
                    match self.clear_run(name) {
                        Ok(()) => true,
                        Err(error) => {
                            self.notice(format!(
                                "Завершение получено, но состояние не сохранено: {error}"
                            ));
                            false
                        }
                    }
                } else {
                    false
                };
                if let Some(task) = self.task_mut(name) {
                    task.status = if let Some(code) = process.remote_exit.filter(|_| saved) {
                        if process.stopping_at.is_some() {
                            TaskStatus::Stopped
                        } else {
                            TaskStatus::Completed(code)
                        }
                    } else {
                        TaskStatus::Unknown
                    };
                    if task.status == TaskStatus::Unknown {
                        task.log.push("[MakePort] Нет подтверждения завершения. Удалённый процесс мог остаться запущенным.");
                        task.log.push("Проверьте сервер. Повторный запуск заблокирован до подтверждения в разделе «Проверить».");
                    } else {
                        task.log.push(format!("[MakePort] {}", task.status.label()));
                    }
                    if let Some(error) = &process.error {
                        task.log.push(format!("[SSH] {error}"));
                    }
                }
            }
        }
    }

    fn tick_current(&mut self) {
        let now = Instant::now();
        let mut finished = Vec::new();
        for (&id, process) in &mut self.processes {
            let connecting_too_long =
                !process.ready && process.started_at.elapsed() > Duration::from_secs(30);
            let discovering_too_long = matches!(process.role, Role::Config | Role::Discover)
                && process.started_at.elapsed() > Duration::from_secs(20);
            let stopping_too_long = process
                .stopping_at
                .is_some_and(|at| at.elapsed() > Duration::from_secs(8));
            if (connecting_too_long || discovering_too_long || stopping_too_long) && !process.forced
            {
                process.error = Some(
                    if stopping_too_long {
                        "Не получено подтверждение за 8 секунд; SSH закрывается"
                    } else {
                        "Превышено время ожидания SSH"
                    }
                    .into(),
                );
                process.terminate();
            }
            if process.poll_finished() {
                finished.push(id);
            }
        }
        for id in finished {
            if let Some(process) = self.processes.remove(&id) {
                self.finished(process);
            }
        }
        if self
            .closing
            .is_some_and(|at| at.elapsed() > Duration::from_secs(12))
        {
            self.processes.clear();
            self.dirty = true;
        }
        if self.link == Link::Stopping && self.processes.is_empty() {
            self.link = Link::Offline;
            self.dirty = true;
        }
        if self.desired && self.retry_at.is_some_and(|at| at <= now) {
            self.retry_at = None;
            if let Err(error) = self.begin_tunnel() {
                self.desired = false;
                self.link = Link::Failed;
                self.notice(error);
            }
            self.dirty = true;
        }
    }

    fn view(&self) -> View {
        let connected = self.link == Link::Online && self.desired;
        let discovering = self
            .processes
            .values()
            .any(|p| matches!(p.role, Role::Config | Role::Discover));
        let can_run = !self.fatal
            && self.closing.is_none()
            && (connected || (!self.desired && self.link != Link::Stopping));
        let log_text = if self.log_name.is_empty() {
            self.log.text()
        } else {
            self.tasks
                .iter()
                .find(|t| t.name == self.log_name)
                .map(|t| t.log.text())
                .unwrap_or_default()
        };
        View {
            project_states: self.config.projects.iter().enumerate().map(|(index, project)| {
                if self.selected == Some(index) { self.session.summary() }
                else { self.sessions.get(&project.id).map(Session::summary).unwrap_or_else(|| "Не подключён".into()) }
            }).collect(),
            can_add: !self.fatal && self.closing.is_none(),
            total_running: self.total_running(),
            can_manage_ports: !self.fatal && self.closing.is_none() && self.project().is_some() && !matches!(self.link, Link::Connecting | Link::Retrying | Link::Stopping),
            port_states: self.effective_ports().iter().map(|p| self.port_state(p.local)).collect(),
            port_manual: std::iter::repeat_n(false, self.configured_ports().len()).chain(self.project().into_iter().flat_map(|p| p.manual_ports.iter().map(|_| true))).collect(),
            notice_serial: self.notice_serial,
            discovering,
            next_action: if self.fatal { "Настройки недоступны. Подробности ошибки помогут восстановить доступ." }
                else if self.project().is_none() { "Начните с добавления проекта. Вам понадобятся адрес сервера и папка проекта." }
                else if self.tasks.iter().any(|t| t.status == TaskStatus::Unknown) { "Не удалось подтвердить завершение задачи. Попросите администратора проверить её перед повторным запуском." }
                else if self.link == Link::Connecting { "Проверяем сервер и папку проекта. Это может занять до 30 секунд. Подключение можно отменить." }
                else if self.link == Link::Retrying { "Связь прервалась. Восстанавливаем доступ; задачи повторно не запускаются." }
                else if self.link == Link::Stopping { "Останавливаем задачи и закрываем подключение. Дождитесь завершения." }
                else if discovering { "Получаем список задач из проекта…" }
                else if self.tasks.iter().any(|t| t.status.active()) { "Задачи выполняются. Выберите «Подробности», чтобы посмотреть результат." }
                else if connected { "Выберите задачу и нажмите «Запустить». После запуска сайта откройте его в разделе «Открыть приложение»." }
                else { "Подключитесь, чтобы получить задачи проекта. Уже известную задачу можно запустить сразу." }.into(),
            projects: self.config.projects.clone(), selected: self.selected,
            connection: match self.link {
                Link::Offline => "Не подключено", Link::Connecting => "Подключение…", Link::Online => "Подключено",
                Link::Retrying => "Восстановление соединения…", Link::Stopping => "Отключение…", Link::Failed => "Ошибка подключения",
            }.into(),
            connected, desired: self.desired, can_change: self.can_change(),
            can_connect: !self.fatal && self.closing.is_none() && self.project().is_some() && self.link != Link::Stopping,
            running: self.tasks.iter().filter(|t| t.status.active()).count(),
            tasks: self.tasks.iter().map(|t| TaskView {
                name: t.name.clone(), description: format!("Команда: {}", t.name),
                title: task_title(&t.name, &t.description), state: t.status.label(),
                active: t.status.active(), can_run: can_run && !t.status.active() && t.status != TaskStatus::Unknown,
                can_stop: matches!(t.status, TaskStatus::Starting | TaskStatus::Running), unknown: t.status == TaskStatus::Unknown,
            }).collect(),
            ports: self.effective_ports(),
            port_descriptions: self.port_descriptions(),
            log_name: self.log_name.clone(), log_text, notice: self.notice.clone(), fatal: self.fatal,
            saved_generation: self.saved_generation, quitting: self.closing.is_some(),
            quit_ready: self.closing.is_some() && self.all_processes_empty(),
        }
    }
}

fn run(commands: Receiver<Action>, updates: SyncSender<View>) {
    let (io_tx, io_rx) = mpsc::sync_channel(256);
    let mut engine = Engine::new(io_tx);
    let mut last_send = Instant::now() - Duration::from_secs(1);
    let mut receiver_alive = true;
    loop {
        match commands.recv_timeout(Duration::from_millis(30)) {
            Ok(action) => {
                if let Err(error) = engine.action(action) {
                    engine.notice(error);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if engine.closing.is_none() {
                    let _ = engine.action(Action::Quit);
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        for _ in 0..64 {
            let Ok(action) = commands.try_recv() else {
                break;
            };
            if let Err(error) = engine.action(action) {
                engine.notice(error);
            }
        }
        for _ in 0..256 {
            let Ok(event) = io_rx.try_recv() else {
                break;
            };
            engine.io(event);
        }
        engine.tick();
        let quitting = engine.closing.is_some() && engine.all_processes_empty();
        if (engine.dirty && last_send.elapsed() >= Duration::from_millis(80)) || quitting {
            if receiver_alive {
                match updates.try_send(engine.view()) {
                    Ok(()) => {
                        engine.dirty = false;
                        last_send = Instant::now();
                        if quitting {
                            break;
                        }
                    }
                    Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => {
                        receiver_alive = false;
                        let _ = engine.action(Action::Quit);
                    }
                }
            } else if quitting {
                break;
            }
        }
        // A vanished UI must not leave background processes alive.
        if !receiver_alive && engine.closing.is_none() {
            let _ = engine.action(Action::Quit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_removes_old_rules_but_preserves_manual_running_and_unknown_tasks() {
        let project = Draft {
            name: "Test".into(),
            host: "devbox".into(),
            path: "~/project".into(),
            tasks: "manual".into(),
            ..Draft::default()
        }
        .into_project()
        .unwrap();
        let (io_tx, _) = mpsc::sync_channel(16);
        let mut engine = Engine {
            session: Session {
                notice: String::new(),
                ..Session::default()
            },
            sessions: BTreeMap::new(),
            config: Config {
                projects: vec![project],
                ..Config::default()
            },
            store: None,
            directory: PathBuf::new(),
            selected: Some(0),
            io_tx,
            next_process: 1,
            closing: None,
            fatal: false,
            saved_generation: 0,
            dirty: false,
        };
        for name in ["old", "manual", "running", "unknown"] {
            engine.add_target(Target {
                name: name.into(),
                description: String::new(),
            });
        }
        engine.task_mut("running").unwrap().status = TaskStatus::Running;
        engine.task_mut("unknown").unwrap().status = TaskStatus::Unknown;
        engine.reconcile_targets(vec![Target {
            name: "new".into(),
            description: "New task".into(),
        }]);
        let names = engine
            .tasks
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["manual", "running", "unknown", "new"]);
        assert!(engine.log_name.is_empty());
        assert!(
            !engine
                .view()
                .tasks
                .iter()
                .find(|t| t.name == "unknown")
                .unwrap()
                .can_run
        );
    }
}
