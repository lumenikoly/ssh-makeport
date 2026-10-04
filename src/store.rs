use crate::model::Config;
use crate::Result;
use directories::ProjectDirs;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Store {
    directory: PathBuf,
    // Do not unlink the lock file: keeping its inode avoids a locking race.
    _lock: File,
}

pub fn config_directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("MAKEPORT_CONFIG_DIR") {
        let path = PathBuf::from(path);
        if !path.is_absolute() { return Err("MAKEPORT_CONFIG_DIR должен быть абсолютным путём".into()); }
        return Ok(path);
    }
    ProjectDirs::from("app", "makeport", "MakePort")
        .map(|p| p.config_dir().to_path_buf())
        .ok_or_else(|| "Не удалось определить папку настроек".into())
}

impl Store {
    pub fn open(directory: &Path) -> Result<(Self, Config)> {
        fs::create_dir_all(directory).map_err(|e| format!("Папка настроек: {e}"))?;
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .open(directory.join("instance.lock")).map_err(|e| format!("Файл блокировки: {e}"))?;
        lock.try_lock_exclusive().map_err(|_| "MakePort уже запущен с этой папкой настроек".to_owned())?;
        let store = Self { directory: directory.to_path_buf(), _lock: lock };
        let path = store.path();
        let config = if path.exists() {
            let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
            if metadata.len() > 1024 * 1024 { return Err("Файл настроек превышает 1 МиБ".into()); }
            let text = fs::read_to_string(&path).map_err(|e| format!("Чтение настроек: {e}"))?;
            let config: Config = toml::from_str(&text)
                .map_err(|e| format!("Ошибка config.toml. Файл не изменён.\n{e}"))?;
            config.validate()?;
            config
        } else { Config::default() };
        Ok((store, config))
    }

    pub fn path(&self) -> PathBuf { self.directory.join("config.toml") }

    pub fn save(&self, config: &Config) -> Result<()> {
        config.validate()?;
        let contents = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
        if contents.len() > 1024 * 1024 {
            return Err(
                "Файл настроек превышает 1 МиБ. Сократите описания или число профилей.".into(),
            );
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory).map_err(|e| e.to_string())?;
        temporary.write_all(contents.as_bytes()).map_err(|e| e.to_string())?;
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        temporary.persist(self.path()).map_err(|e| format!("Сохранение настроек: {}", e.error))?;
        #[cfg(unix)]
        if let Ok(parent) = File::open(&self.directory) { let _ = parent.sync_all(); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn oversized_settings_never_replace_the_readable_configuration() {
        use crate::model::{Draft, ManualPort};
        let temp = tempfile::tempdir().unwrap();
        let (store, mut config) = super::Store::open(temp.path()).unwrap();
        store.save(&config).unwrap();
        let original = std::fs::read(store.path()).unwrap();
        for index in 0..64 {
            let mut project = Draft {
                id: format!("p{index}"),
                name: "Project".into(),
                host: "devbox".into(),
                path: format!("/{}", "a".repeat(3900)),
                ..Draft::default()
            }
            .into_project()
            .unwrap();
            project.manual_ports = (1000..1032)
                .map(|local| ManualPort {
                    local,
                    remote: 80,
                    description: "a".repeat(500),
                })
                .collect();
            config.projects.push(project);
        }
        config.validate().unwrap();
        assert!(store.save(&config).unwrap_err().contains("1 МиБ"));
        assert_eq!(std::fs::read(store.path()).unwrap(), original);
    }

    use super::*;
    #[test]
    fn config_round_trip_is_atomic_and_readable() {
        let temp = tempfile::tempdir().unwrap();
        let (store, config) = Store::open(temp.path()).unwrap();
        store.save(&config).unwrap();
        store.save(&config).unwrap();
        drop(store);
        let (_, loaded) = Store::open(temp.path()).unwrap();
        assert_eq!(loaded.version, 1);
    }
    #[test]
    fn unconfirmed_runs_survive_restart() {
        use crate::model::{Draft, RunRecord};
        let temp = tempfile::tempdir().unwrap();
        let (store, mut config) = Store::open(temp.path()).unwrap();
        let project = Draft { name: "Demo".into(), host: "devbox".into(), path: "~/project".into(), ..Draft::default() }.into_project().unwrap();
        config.unconfirmed.push(RunRecord { project_id: project.id.clone(), target: "dev".into() });
        config.projects.push(project);
        store.save(&config).unwrap();
        drop(store);
        let (_, loaded) = Store::open(temp.path()).unwrap();
        assert_eq!(loaded.unconfirmed.len(), 1);
        assert_eq!(loaded.unconfirmed[0].target, "dev");
    }
    #[test]
    fn second_instance_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let (_store, _) = Store::open(temp.path()).unwrap();
        assert!(Store::open(temp.path()).is_err());
    }
    #[test]
    fn corrupted_config_is_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.toml"), "broken = [").unwrap();
        assert!(Store::open(temp.path()).is_err());
        assert_eq!(fs::read_to_string(temp.path().join("config.toml")).unwrap(), "broken = [");
    }
}
