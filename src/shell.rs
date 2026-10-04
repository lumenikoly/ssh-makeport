use crate::model::{validate_target, Forward, Project};
use crate::Result;

pub const CONFIG_SCRIPT: &str = include_str!("../remote/config.sh");

pub const CONNECT_SCRIPT: &str = include_str!("../remote/connect.sh");
pub const DISCOVER_SCRIPT: &str = include_str!("../remote/discover.sh");
pub const TASK_SCRIPT: &str = include_str!("../remote/task.sh");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    Tunnel,
    Port(Forward),
    Config,
    Discover,
    Task(String),
}

pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn ready_marker(token: &str) -> String {
    format!("__MAKEPORT_READY_{token}__")
}
pub fn started_marker(token: &str) -> String {
    format!("__MAKEPORT_STARTED_{token}__")
}
pub fn exit_marker(token: &str) -> String {
    format!("__MAKEPORT_EXIT_{token}__:")
}

pub fn remote_command(project: &Project, role: &Role, token: &str) -> Result<String> {
    project.validate()?;
    let script = match role {
        Role::Tunnel | Role::Port(_) => CONNECT_SCRIPT,
        Role::Config => CONFIG_SCRIPT,
        Role::Discover => DISCOVER_SCRIPT,
        Role::Task(target) => {
            validate_target(target)?;
            TASK_SCRIPT
        }
    };
    let mut command = format!(
        "sh -c {} makeport {} {}",
        quote(script),
        quote(&project.path),
        quote(token)
    );
    if let Role::Task(target) = role {
        command.push_str(&format!(" {}", quote(target)));
    }
    Ok(command)
}

pub fn arguments(project: &Project, role: &Role, token: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    // Session ownership is independent of ~/.ssh/config ControlMaster settings.
    // No interactive password dialogs and no implicit trust of new host keys.
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "ConnectTimeout=10",
        "ConnectionAttempts=1",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
        "ControlMaster=no",
        "ControlPath=none",
        "ControlPersist=no",
        "ExitOnForwardFailure=yes",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RemoteCommand=none",
        "ForkAfterAuthentication=no",
        "StdinNull=no",
        "SessionType=default",
        "EscapeChar=none",
        "LogLevel=ERROR",
    ] {
        args.push("-o".into());
        args.push(option.into());
    }
    match role {
        Role::Tunnel => {
            args.extend(["-T".into(), "-o".into(), "ClearAllForwardings=no".into()]);
            for port in &project.ports {
                args.extend(["-L".into(), port.argument()]);
            }
        }
        Role::Port(port) => {
            args.extend([
                "-T".into(),
                "-o".into(),
                "ClearAllForwardings=no".into(),
                "-L".into(),
                port.argument(),
            ]);
        }
        Role::Config | Role::Discover => {
            args.extend(["-T".into(), "-o".into(), "ClearAllForwardings=yes".into()])
        }
        Role::Task(_) => args.extend(["-tt".into(), "-o".into(), "ClearAllForwardings=yes".into()]),
    }
    args.push(project.host.clone());
    args.push(remote_command(project, role, token)?);
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Forward;
    fn project() -> Project {
        Project {
            manual_ports: Vec::new(),
            id: "a".into(),
            name: "test".into(),
            host: "dev".into(),
            path: "~/my ' project".into(),
            ports: vec![Forward {
                local: 5173,
                remote: 80,
            }],
            tasks: Vec::new(),
            reconnect: true,
        }
    }
    #[test]
    fn quotes_single_quotes_and_leaves_expansion_literal() {
        assert_eq!(quote("a'b"), "'a'\\''b'");
        assert_eq!(quote("$(id)"), "'$(id)'");
    }
    #[test]
    fn tunnel_bind_is_explicitly_loopback() {
        let args = arguments(&project(), &Role::Tunnel, "x").unwrap();
        assert!(args.contains(&"127.0.0.1:5173:127.0.0.1:80".into()));
        assert!(args.contains(&"StrictHostKeyChecking=yes".into()));
        assert!(!args.contains(&"-N".into()));
    }
    #[test]
    fn task_forces_remote_pty_and_disables_inherited_forwards() {
        let args = arguments(&project(), &Role::Task("dev".into()), "x").unwrap();
        assert!(args.contains(&"-tt".into()));
        assert!(args.contains(&"ClearAllForwardings=yes".into()));
        assert!(!args.contains(&"-L".into()));
    }
    #[test]
    fn discovery_only_reads_a_file() {
        assert!(!DISCOVER_SCRIPT.contains("make -"));
        assert!(DISCOVER_SCRIPT.contains("head -c 262145"));
    }
}
