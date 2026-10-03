use crate::model::{validate_target, Forward, MAX_PORTS, MAX_TASKS};
use crate::Result;
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectFile {
    #[serde(default)]
    pub ports: Vec<Port>,
    #[serde(default)]
    pub tasks: Vec<Task>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub remote: u16,
    pub local: Option<u16>,
    pub description: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub target: String,
    pub description: String,
}
impl ProjectFile {
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > 65536 {
            return Err("makeport.toml больше 64 КиБ".into());
        }
        let file: Self =
            toml::from_str(text).map_err(|e| format!("Ошибка в makeport.toml: {e}"))?;
        if file.ports.len() > MAX_PORTS || file.tasks.len() > MAX_TASKS {
            return Err("В makeport.toml слишком много портов или задач".into());
        }
        let mut ports = HashSet::new();
        let mut tasks = HashSet::new();
        for port in &file.ports {
            if port.remote == 0
                || port.local == Some(0)
                || !ports.insert(port.local.unwrap_or(port.remote))
            {
                return Err(
                    "В makeport.toml порт равен нулю или локальный порт повторяется".into(),
                );
            }
            description(&port.description)?;
        }
        for task in &file.tasks {
            validate_target(&task.target)?;
            if !tasks.insert(&task.target) {
                return Err("В makeport.toml задача повторяется".into());
            }
            description(&task.description)?;
        }
        Ok(file)
    }
    pub fn forwards(&self) -> Vec<Forward> {
        self.ports
            .iter()
            .map(|p| Forward {
                local: p.local.unwrap_or(p.remote),
                remote: p.remote,
            })
            .collect()
    }
}
fn description(text: &str) -> Result<()> {
    if text.trim().is_empty() || text.len() > 500 || text.chars().any(char::is_control) {
        return Err("Укажите описание длиной до 500 байт без переносов строк".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_descriptions() {
        let file = ProjectFile::parse(
            r#"[[ports]]
remote = 5173
description = "Сайт"
[[ports]]
remote = 5432
local = 15432
description = "База данных"
[[tasks]]
target = "dev"
description = "Запустить сайт"
"#,
        )
        .unwrap();
        assert_eq!(
            file.forwards(),
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
        assert_eq!(file.tasks[0].description, "Запустить сайт");
        assert!(ProjectFile::parse("").unwrap().tasks.is_empty());
    }
    #[test]
    fn rejects_unsafe_and_ambiguous_configuration() {
        for text in [
            "[[tasks]]\ntarget='dev;id'\ndescription='Run'",
            "[[ports]]\nremote=0\ndescription='Site'",
            "[[ports]]\nremote=80\ndescription='Site'\n[[ports]]\nremote=81\nlocal=80\ndescription='Other'",
            "[[tasks]]\ntarget='dev'\ndescription='Run'\n[[tasks]]\ntarget='dev'\ndescription='Again'",
            "[[tasks]]\ntarget='dev'\ndescription=''",
            "command='rm -rf .'",
        ] { assert!(ProjectFile::parse(text).is_err(), "{text}"); }
        assert!(ProjectFile::parse(&"x".repeat(65537)).is_err());
    }
}
