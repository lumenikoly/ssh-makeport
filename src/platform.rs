use crate::Result;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn ssh_executable() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("MAKEPORT_SSH") {
        let path = PathBuf::from(value);
        if path.is_absolute() && path.is_file() { return Ok(path); }
        return Err("MAKEPORT_SSH должен указывать на существующий исполняемый файл абсолютным путём".into());
    }
    #[cfg(target_os = "macos")]
    if Path::new("/usr/bin/ssh").is_file() { return Ok(PathBuf::from("/usr/bin/ssh")); }
    #[cfg(target_os = "windows")]
    {
        if let Some(root) = std::env::var_os("SystemRoot") {
            let path = PathBuf::from(root).join("System32").join("OpenSSH").join("ssh.exe");
            if path.is_file() { return Ok(path); }
        }
    }
    let file = if cfg!(windows) { "ssh.exe" } else { "ssh" };
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            // Never search the working directory, especially on Windows.
            if directory.is_absolute() {
                let candidate = directory.join(file);
                if candidate.is_file() { return Ok(candidate); }
            }
        }
    }
    Err("OpenSSH не найден. Установите SSH-клиент или задайте MAKEPORT_SSH. Инструкция — в README.md".into())
}

pub fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW; remote PTY still works.
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn open_with(mut command: Command) -> Result<()> {
    hide_console(&mut command);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = command.spawn().map_err(|e| format!("Не удалось открыть: {e}"))?;
    // Reap the launcher without blocking the UI/engine.
    std::thread::spawn(move || { let _ = child.wait(); });
    Ok(())
}

pub fn open_port(port: u16) -> Result<()> {
    if port == 0 { return Err("Некорректный порт".into()); }
    let url = format!("http://127.0.0.1:{port}/");
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    #[cfg(windows)]
    let mut command = {
        let root = std::env::var_os("SystemRoot").ok_or("SystemRoot не задан")?;
        let mut command = Command::new(PathBuf::from(root).join("System32").join("rundll32.exe"));
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    command.arg(url);
    open_with(command)
}

pub fn open_directory(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    #[cfg(windows)]
    let mut command = {
        let root = std::env::var_os("SystemRoot").ok_or("SystemRoot не задан")?;
        Command::new(PathBuf::from(root).join("explorer.exe"))
    };
    command.arg(path);
    open_with(command)
}
