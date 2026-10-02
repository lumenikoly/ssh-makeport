use crate::log::{AnsiFilter, LineDecoder};
use crate::platform::hide_console;
use crate::shell::Role;
use crate::Result;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub enum Stream { Stdout, Stderr }
impl Stream { pub fn index(self) -> usize { match self { Self::Stdout => 0, Self::Stderr => 1 } } }

#[derive(Debug)]
pub enum IoEvent {
    Data(u64, Stream, Vec<u8>),
    Closed(u64, Stream),
    Error(u64, Stream, String),
}

fn reader(id: u64, stream: Stream, mut source: impl Read + Send + 'static, tx: SyncSender<IoEvent>) -> Result<()> {
    std::thread::Builder::new().name(format!("ssh-output-{id}-{}", stream.index()))
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                match source.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => if tx.send(IoEvent::Data(id, stream, buffer[..count].to_vec())).is_err() { return; },
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => { let _ = tx.send(IoEvent::Error(id, stream, error.to_string())); break; }
                }
            }
            let _ = tx.send(IoEvent::Closed(id, stream));
        }).map(|_| ()).map_err(|e| format!("Поток чтения SSH: {e}"))
}

pub struct Process {
    pub id: u64,
    pub role: Role,
    pub token: String,
    child: Child,
    input: Option<ChildStdin>,
    pub started_at: Instant,
    pub stopping_at: Option<Instant>,
    pub forced: bool,
    pub ready: bool,
    pub remote_exit: Option<i32>,
    pub capture: Vec<u8>,
    pub capture_overflow: bool,
    pub decoders: [LineDecoder; 2],
    pub filters: [AnsiFilter; 2],
    closed: [bool; 2],
    pub status: Option<ExitStatus>,
    exited_at: Option<Instant>,
    pub error: Option<String>,
}

impl Process {
    pub fn spawn(id: u64, role: Role, token: String, executable: &Path, args: &[String], tx: SyncSender<IoEvent>) -> Result<Self> {
        let mut command = Command::new(executable);
        command.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide_console(&mut command);
        let mut child = command.spawn().map_err(|e| format!("Запуск {}: {e}", executable.display()))?;
        let input = child.stdin.take();
        let stdout = child.stdout.take().ok_or("Не удалось получить stdout SSH")?;
        let stderr = child.stderr.take().ok_or("Не удалось получить stderr SSH")?;
        let readers = reader(id, Stream::Stdout, stdout, tx.clone())
            .and_then(|_| reader(id, Stream::Stderr, stderr, tx));
        if let Err(error) = readers { let _ = child.kill(); let _ = child.wait(); return Err(error); }
        Ok(Self {
            id, role, token, child, input, started_at: Instant::now(), stopping_at: None,
            forced: false, ready: false, remote_exit: None, capture: Vec::new(), capture_overflow: false,
            decoders: Default::default(), filters: Default::default(), closed: [false; 2],
            status: None, exited_at: None, error: None,
        })
    }

    pub fn close_stream(&mut self, stream: Stream) { self.closed[stream.index()] = true; }

    pub fn request_stop(&mut self) -> Result<()> {
        if self.stopping_at.is_some() { return Ok(()); }
        self.stopping_at = Some(Instant::now());
        match self.role {
            Role::Task(_) => self.input.as_mut().ok_or("SSH stdin уже закрыт")?
                .write_all(&[3]).map_err(|e| format!("Передача Ctrl+C: {e}")),
            _ => { self.input.take(); Ok(()) }
        }
    }

    pub fn terminate(&mut self) {
        if self.forced { return; }
        self.forced = true;
        self.input.take();
        if let Err(error) = self.child.kill() { self.error = Some(format!("Закрытие SSH: {error}")); }
    }

    pub fn poll_finished(&mut self) -> bool {
        if self.exited_at.is_none() {
            match self.child.try_wait() {
                Ok(Some(status)) => { self.status = Some(status); self.exited_at = Some(Instant::now()); }
                Ok(None) => {}
                Err(error) => {
                    self.error = Some(format!("Ожидание SSH: {error}"));
                    self.terminate();
                    self.exited_at = Some(Instant::now());
                }
            }
        }
        self.exited_at.is_some_and(|at| self.closed.iter().all(|closed| *closed) || at.elapsed() > Duration::from_secs(2))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.input.take();
        if self.status.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
