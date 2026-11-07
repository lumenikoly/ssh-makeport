use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_PROJECTS: usize = 64;
pub const MAX_TASKS: usize = 128;
pub const MAX_RUNNING: usize = 8;
pub const MAX_PORTS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub unconfirmed: Vec<RunRecord>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            selected: None,
            projects: Vec::new(),
            unconfirmed: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub project_id: String,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    // Read-only compatibility with profiles created before the unified config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manual_ports: Vec<Port>,
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "home")]
    pub path: String,
    #[serde(default)]
    pub ports: Vec<Port>,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default = "yes")]
    pub reconnect: bool,
}

fn yes() -> bool {
    true
}

fn home() -> String {
    "~".into()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forward {
    pub local: u16,
    pub remote: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub local: u16,
    pub remote: u16,
    #[serde(default)]
    pub description: String,
}
impl Port {
    pub fn forward(&self) -> Forward {
        Forward {
            local: self.local,
            remote: self.remote,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.local == 0 || self.remote == 0 {
            return Err("Порт должен быть от 1 до 65535".into());
        }
        if self.description.len() > 500 || self.description.chars().any(char::is_control) {
            return Err("Укажите описание длиной до 500 байт без переносов строк".into());
        }
        Ok(())
    }
}

impl Forward {
    pub fn argument(&self) -> String {
        format!("127.0.0.1:{}:127.0.0.1:{}", self.local, self.remote)
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.local)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub id: String,
    pub name: String,
    pub host: String,
    pub path: String,
    pub ports: String,
    pub tasks: String,
    pub reconnect: bool,
}

impl Draft {
    pub fn into_project(self) -> Result<Project> {
        let project = Project {
            manual_ports: Vec::new(),
            id: if self.id.is_empty() {
                unique_id()
            } else {
                self.id
            },
            name: self.name.trim().to_owned(),
            host: self.host.trim().to_owned(),
            path: if self.path.trim().is_empty() {
                home()
            } else {
                self.path.trim().to_owned()
            },
            ports: parse_ports(&self.ports)?
                .into_iter()
                .map(|p| Port {
                    local: p.local,
                    remote: p.remote,
                    description: String::new(),
                })
                .collect(),
            tasks: parse_tasks(&self.tasks)?,
            reconnect: self.reconnect,
        };
        project.validate()?;
        Ok(project)
    }
}

impl From<&Project> for Draft {
    fn from(project: &Project) -> Self {
        Self {
            id: project.id.clone(),
            name: project.name.clone(),
            host: project.host.clone(),
            path: project.path.clone(),
            ports: project
                .ports
                .iter()
                .map(|p| format!("{}:{}", p.local, p.remote))
                .collect::<Vec<_>>()
                .join(", "),
            tasks: project.tasks.join(", "),
            reconnect: project.reconnect,
        }
    }
}

impl Config {
    pub fn migrate(&mut self) {
        for project in &mut self.projects {
            project.ports.append(&mut project.manual_ports);
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err("Неизвестная версия конфигурации".into());
        }
        if self.projects.len() > MAX_PROJECTS {
            return Err("Слишком много проектов".into());
        }
        let mut ids = HashSet::new();
        for project in &self.projects {
            project.validate()?;
            if !ids.insert(&project.id) {
                return Err("Повторяющийся идентификатор проекта".into());
            }
        }
        if self.unconfirmed.len() > MAX_TASKS {
            return Err("Слишком много неподтверждённых запусков".into());
        }
        let mut runs = HashSet::new();
        for run in &self.unconfirmed {
            validate_target(&run.target)?;
            if !self.projects.iter().any(|p| p.id == run.project_id) {
                return Err("Неподтверждённый запуск ссылается на неизвестный проект".into());
            }
            if !runs.insert((&run.project_id, &run.target)) {
                return Err("Повторяющаяся запись неподтверждённого запуска".into());
            }
        }
        Ok(())
    }
}

impl Project {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.id.len() > 80
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("Некорректный идентификатор проекта".into());
        }
        if self.name.is_empty() || self.name.len() > 200 || self.name.chars().any(char::is_control)
        {
            return Err("Укажите название проекта (до 200 байт)".into());
        }
        validate_host(&self.host)?;
        validate_path(&self.path)?;
        if self.ports.len() + self.manual_ports.len() > MAX_PORTS {
            return Err(format!("Допускается не более {MAX_PORTS} туннелей"));
        }
        let mut locals = HashSet::new();
        for port in &self.ports {
            port.validate()?;
            if !locals.insert(port.local) {
                return Err(format!("Локальный порт {} указан дважды", port.local));
            }
        }
        for port in &self.manual_ports {
            port.validate()?;
            if !locals.insert(port.local) {
                return Err(format!("Локальный порт {} указан дважды", port.local));
            }
        }
        if self.tasks.len() > MAX_TASKS {
            return Err("Слишком много задач".into());
        }
        for task in &self.tasks {
            validate_target(task)?;
        }
        Ok(())
    }
}

pub fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || host.len() > 255
        || host.starts_with('-')
        || host.starts_with('@')
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@:%[]".contains(&b))
    {
        return Err(
            "Укажите SSH-псевдоним или user@host без пробелов. Порт SSH задаётся в ~/.ssh/config"
                .into(),
        );
    }
    Ok(())
}

pub fn validate_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.chars().any(char::is_control)
        || !(path == "~" || path.starts_with("~/") || path.starts_with('/'))
    {
        return Err("Путь должен начинаться с / или ~/; например ~/projects/site".into());
    }
    Ok(())
}

pub fn validate_target(target: &str) -> Result<()> {
    if target.is_empty()
        || target.len() > 128
        || target.starts_with('-')
        || !target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
    {
        return Err(
            "Имя цели: буквы, цифры, _, -, . и /; без пробелов, параметров и переменных".into(),
        );
    }
    Ok(())
}

pub fn parse_ports(input: &str) -> Result<Vec<Forward>> {
    let mut ports = Vec::new();
    let mut used = HashSet::new();
    for entry in input
        .split([',', ';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let (left, right) = entry.split_once(':').unwrap_or((entry, entry));
        let parse = |value: &str| -> Result<u16> {
            value
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| format!("Некорректный порт: {value}"))
        };
        let range = |text: &str| -> Result<(u16, u16)> {
            let (start, end) = text.split_once('-').unwrap_or((text, text));
            let (start, end) = (parse(start)?, parse(end)?);
            if start > end {
                return Err(format!("Обратный диапазон портов: {text}"));
            }
            Ok((start, end))
        };
        let (local, local_end) = range(left)?;
        let (remote, remote_end) = range(right)?;
        if local_end - local != remote_end - remote {
            return Err(
                "Диапазоны локальных и удалённых портов должны быть одинаковой длины".into(),
            );
        }
        let count = usize::from(local_end - local) + 1;
        if ports.len() + count > MAX_PORTS {
            return Err(format!("Допускается не более {MAX_PORTS} туннелей"));
        }
        for offset in 0..count as u16 {
            let port = Forward {
                local: local + offset,
                remote: remote + offset,
            };
            if !used.insert(port.local) {
                return Err(format!("Порт {} указан дважды", port.local));
            }
            ports.push(port);
        }
    }
    if ports.len() > MAX_PORTS {
        return Err(format!("Допускается не более {MAX_PORTS} туннелей"));
    }
    Ok(ports)
}

pub fn parse_tasks(input: &str) -> Result<Vec<String>> {
    let mut tasks = Vec::new();
    for task in input
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
    {
        validate_target(task)?;
        if !tasks.iter().any(|s| s == task) {
            tasks.push(task.to_owned());
        }
    }
    if tasks.len() > MAX_TASKS {
        return Err("Слишком много задач".into());
    }
    Ok(tasks)
}

// Correlation identifiers, not authentication secrets. The SSH server is trusted.
pub fn unique_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{:x}-{:x}-{:x}",
        time,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn profiles_from_v02_load_and_validate_manual_port_extensions() {
        let mut project: super::Project = toml::from_str("id='legacy'\nname='Legacy'\nhost='devbox'\npath='~/project'\nports=[{local=5173,remote=5173}]\ntasks=['dev']\nreconnect=true\n").unwrap();
        assert!(project.manual_ports.is_empty());
        project.manual_ports.push(super::Port {
            local: 15432,
            remote: 5432,
            description: "Database".into(),
        });
        project.validate().unwrap();
        let roundtrip: super::Project =
            toml::from_str(&toml::to_string(&project).unwrap()).unwrap();
        assert_eq!(roundtrip, project);
        project.manual_ports[0].local = 5173;
        assert!(project.validate().is_err());
        project.manual_ports[0].local = 0;
        assert!(project.validate().is_err());
    }

    use super::*;

    #[test]
    fn ports_support_shorthand_and_mapping() {
        assert_eq!(
            parse_ports("5173, 15432:5432").unwrap(),
            vec![
                Forward {
                    local: 5173,
                    remote: 5173
                },
                Forward {
                    local: 15432,
                    remote: 5432
                }
            ]
        );
    }
    #[test]
    fn rejects_invalid_ports() {
        for input in [
            "0",
            "65536",
            "a",
            "1:2:3",
            "5173,5173:80",
            "-1",
            "3-1",
            "1-3:5-6",
            "1-65535",
            "1-3,3",
        ] {
            assert!(parse_ports(input).is_err(), "{input}");
        }
    }
    #[test]
    fn expands_port_ranges_with_mapping_and_limits() {
        let ports = parse_ports("13000-13255:3000-3255").unwrap();
        assert_eq!(ports.len(), MAX_PORTS);
        assert_eq!(
            ports.last(),
            Some(&Forward {
                local: 13255,
                remote: 3255
            })
        );
        assert_eq!(parse_ports("65534-65535").unwrap().len(), 2);
        assert!(parse_ports("1-257").is_err());
    }
    #[test]
    fn rejects_option_and_shell_injection() {
        for input in ["-f", "dev;touch /tmp/x", "$(id)", "A=B", "dev\ntest", "a b"] {
            assert!(validate_target(input).is_err(), "{input}");
        }
        for input in ["-oProxyCommand=evil", "host;id", "user@host x", "$(id)", ""] {
            assert!(validate_host(input).is_err(), "{input}");
        }
    }
    #[test]
    fn host_supports_alias_user_ipv6() {
        for input in ["devbox", "me@dev-box", "user@192.168.1.3", "user@[::1]"] {
            assert!(validate_host(input).is_ok(), "{input}");
        }
    }
    #[test]
    fn paths_allow_literal_shell_characters_but_not_controls() {
        assert!(validate_path("~/my project/'$(not-executed)'").is_ok());
        for input in ["relative/path", "~other/path", "/x\nfoo", ""] {
            assert!(validate_path(input).is_err());
        }
    }
}
