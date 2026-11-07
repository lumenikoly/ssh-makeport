// Offline subprocess fixture: binds real loopback listeners and emits the SSH protocol.
// It deliberately does not emulate authentication or remote shell execution.
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let command = args.last().unwrap();
    let parts = command.rsplit('\'').collect::<Vec<_>>();
    let task = args.iter().any(|a| a == "-tt");
    let token = parts[if task { 3 } else { 1 }];
    let path = parts[if task { 5 } else { 3 }];
    let directory = std::path::PathBuf::from(std::env::var_os("MAKEPORT_TEST_REMOTE").unwrap())
        .join(path.trim_start_matches('/'));
    let kind = if task { "task" } else { "tunnel" };
    let count = args.iter().filter(|a| *a == "-L").count();
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.parent().unwrap().join("invocations"))
        .unwrap();
    writeln!(log, "{kind}:{count}").unwrap();
    assert!(
        !command.contains("head -c"),
        "Remote discovery is forbidden"
    );
    if task {
        let target = parts[1];
        println!("\n__MAKEPORT_STARTED_{token}__");
        println!("TASK:{path}:{target}");
        std::io::stdout().flush().unwrap();
        if target == "unknown" {
            std::process::exit(255);
        }
        if target == "dev" {
            let mut byte = [0];
            if std::io::stdin().read_exact(&mut byte).is_ok() && byte[0] == 3 {
                println!("\n__MAKEPORT_EXIT_{token}__:2");
            }
        } else {
            println!("\n__MAKEPORT_EXIT_{token}__:0");
        }
    } else {
        let mut listeners = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            if arg == "-L" {
                let mapping = args[i + 1].split(':').collect::<Vec<_>>();
                let fail = directory.parent().unwrap().join("fail-local");
                if fs::read_to_string(&fail).ok().as_deref() == Some(mapping[1]) {
                    eprintln!("cannot listen: simulated SSH forwarding failure");
                    std::process::exit(255);
                }

                match TcpListener::bind((mapping[0], mapping[1].parse::<u16>().unwrap())) {
                    Ok(listener) => listeners.push(listener),
                    Err(error) => {
                        eprintln!("cannot listen: {error}");
                        std::process::exit(255);
                    }
                }
            }
        }
        println!("\n__MAKEPORT_READY_{token}__");
        std::io::stdout().flush().unwrap();
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input).unwrap();
        drop(listeners);
    }
}
