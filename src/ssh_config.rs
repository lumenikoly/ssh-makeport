//! Enumerate literal SSH aliases without evaluating Match exec or opening connections.
use crate::model::validate_host;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_FILE: usize = 256 * 1024;
const MAX_TOTAL: usize = 2 * 1024 * 1024;
const MAX_FILES: usize = 64;
const MAX_HOSTS: usize = 512;

#[derive(Default)]
pub struct Discovery {
    pub hosts: Vec<String>,
    pub warning: String,
}

pub fn discover() -> Discovery {
    let Some(dirs) = directories::BaseDirs::new() else {
        return Discovery::default();
    };
    let home = dirs.home_dir();
    let user = home.join(".ssh");
    #[cfg(windows)]
    let system = std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("ssh"));
    #[cfg(not(windows))]
    let system = Some(PathBuf::from("/etc/ssh"));
    let mut roots = vec![(user.join("config"), user)];
    if let Some(system) = system {
        roots.push((system.join("ssh_config"), system));
    }
    read_configs(home, &roots)
}

fn read_configs(home: &Path, roots: &[(PathBuf, PathBuf)]) -> Discovery {
    let mut reader = Reader {
        home,
        visited: HashSet::new(),
        aliases: HashSet::new(),
        hosts: Vec::new(),
        bytes: 0,
        incomplete: false,
    };
    for (path, base) in roots {
        reader.read(path, base, 0);
    }
    reader.hosts.sort();
    Discovery {
        hosts: reader.hosts,
        warning: if reader.incomplete {
            "Не все SSH-настройки удалось прочитать. Сервер можно указать вручную.".into()
        } else {
            String::new()
        },
    }
}

struct Reader<'a> {
    home: &'a Path,
    visited: HashSet<PathBuf>,
    aliases: HashSet<String>,
    hosts: Vec<String>,
    bytes: usize,
    incomplete: bool,
}
impl Reader<'_> {
    fn read(&mut self, path: &Path, base: &Path, depth: usize) {
        if depth > 16 || self.visited.len() >= MAX_FILES || self.bytes >= MAX_TOTAL {
            self.incomplete = true;
            return;
        }
        let path = match std::fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) => {
                self.incomplete |= error.kind() != std::io::ErrorKind::NotFound;
                return;
            }
        };
        if !self.visited.insert(path.clone()) {
            return;
        }
        let mut bytes = Vec::new();
        let result = std::fs::File::open(&path).and_then(|file| {
            file.take((MAX_FILE.min(MAX_TOTAL - self.bytes) + 1) as u64)
                .read_to_end(&mut bytes)
        });
        if result.is_err() || bytes.len() > MAX_FILE || self.bytes + bytes.len() > MAX_TOTAL {
            self.incomplete = true;
            return;
        }
        self.bytes += bytes.len();
        for line in String::from_utf8_lossy(&bytes)
            .trim_start_matches('\u{feff}')
            .lines()
        {
            let line = line.trim();
            let end = line
                .find(|c: char| c.is_whitespace() || c == '=')
                .unwrap_or(line.len());
            let key = &line[..end];
            if !key.eq_ignore_ascii_case("host") && !key.eq_ignore_ascii_case("include") {
                continue;
            }
            let values = line[end..]
                .trim_start()
                .strip_prefix('=')
                .unwrap_or(line[end..].trim_start())
                .trim_start();
            let Some(values) = words(values) else {
                self.incomplete = true;
                continue;
            };
            if key.eq_ignore_ascii_case("host") {
                for host in &values {
                    if self.hosts.len() >= MAX_HOSTS {
                        self.incomplete = true;
                        break;
                    }
                    if host.starts_with('!')
                        || host.contains(['*', '?'])
                        || validate_host(host).is_err()
                        || values
                            .iter()
                            .filter_map(|v| v.strip_prefix('!'))
                            .any(|p| matches_host(p, host))
                    {
                        continue;
                    }
                    if self.aliases.insert(host.clone()) {
                        self.hosts.push(host.clone());
                    }
                }
            } else {
                for value in values {
                    let Some(path) = expand_path(&value, self.home) else {
                        self.incomplete = true;
                        continue;
                    };
                    let path = if path.is_absolute() {
                        path
                    } else {
                        base.join(path)
                    };
                    match glob::glob(&path.to_string_lossy()) {
                        Ok(paths) => {
                            for path in paths {
                                if self.visited.len() >= MAX_FILES || self.bytes >= MAX_TOTAL {
                                    self.incomplete = true;
                                    break;
                                }
                                match path {
                                    Ok(path) => self.read(&path, base, depth + 1),
                                    Err(_) => self.incomplete = true,
                                }
                            }
                        }
                        Err(_) => self.incomplete = true,
                    }
                }
            }
        }
    }
}

fn words(line: &str) -> Option<Vec<String>> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => quoted = !quoted,
            '#' if !quoted => break,
            '\\' if chars
                .peek()
                .is_some_and(|c| *c == '"' || *c == '\\' || c.is_whitespace()) =>
            {
                word.push(chars.next()?)
            }
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    result.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if quoted {
        return None;
    }
    if !word.is_empty() {
        result.push(word);
    }
    Some(result)
}

fn expand_path(value: &str, home: &Path) -> Option<PathBuf> {
    let mut expanded = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let end = rest[start + 2..].find('}')? + start + 2;
        expanded.push_str(&std::env::var(&rest[start + 2..end]).ok()?);
        rest = &rest[end + 1..];
    }
    expanded.push_str(rest);
    let mut path = String::new();
    let mut chars = expanded.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            match chars.next()? {
                '%' => path.push('%'),
                'd' => path.push_str(&home.to_string_lossy()),
                _ => return None,
            }
        } else {
            path.push(ch);
        }
    }
    if path == "~" {
        return Some(home.to_path_buf());
    }
    if path.starts_with("~/") || path.starts_with("~\\") {
        return Some(home.join(&path[2..]));
    }
    if path.starts_with('~') {
        return None;
    }
    Some(PathBuf::from(path))
}

fn matches_host(pattern: &str, host: &str) -> bool {
    let mut previous = vec![false; host.len() + 1];
    previous[0] = true;
    for p in pattern.bytes() {
        let mut next = vec![false; host.len() + 1];
        if p == b'*' {
            next[0] = previous[0];
        }
        for (i, h) in host.bytes().enumerate() {
            next[i + 1] = if p == b'*' {
                previous[i + 1] || next[i]
            } else {
                previous[i] && (p == b'?' || p == h)
            };
        }
        previous = next;
    }
    previous[host.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn discover_text(text: &str) -> Discovery {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        fs::write(&config, text).unwrap();
        read_configs(dir.path(), &[(config, dir.path().to_path_buf())])
    }

    #[test]
    fn aliases_accept_config_syntax_but_exclude_patterns_and_negations() {
        let result = discover_text("\u{feff}# config\r\n  hOsT=alpha beta \"quoted\" # comment\r\nHost * *.internal !blocked\nHost alpha beta blocked !block*\nHost Prod prod\nHost -oBadCommand invalid;host\nHostName not-an-alias\nMatch exec \"touch SHOULD_NOT_EXIST\"\n  User me\n");
        assert_eq!(result.hosts, ["Prod", "alpha", "beta", "prod", "quoted"]);
        assert!(result.warning.is_empty());
    }

    #[test]
    fn includes_are_relative_to_ssh_directory_even_when_nested() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join(".ssh");
        fs::create_dir_all(base.join("conf.d")).unwrap();
        fs::write(
            base.join("config"),
            "Host main\nInclude conf.d/*.conf\nInclude missing*\n",
        )
        .unwrap();
        fs::write(
            base.join("conf.d/one.conf"),
            "Host included\nInclude nested.conf\nInclude config\n",
        )
        .unwrap();
        fs::write(base.join("nested.conf"), "Host nested\n").unwrap();
        let result = read_configs(dir.path(), &[(base.join("config"), base)]);
        assert_eq!(result.hosts, ["included", "main", "nested"]);
        assert!(result.warning.is_empty());
    }

    #[test]
    fn quoted_paths_home_tokens_and_system_configs_are_supported() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("with spaces.conf"), "Host spaces\n").unwrap();
        fs::write(dir.path().join("other.conf"), "Host other\n").unwrap();
        fs::write(
            dir.path().join("config"),
            "Include \"~/with spaces.conf\" %d/other.conf\nHost shared\n",
        )
        .unwrap();
        fs::write(dir.path().join("system"), "Host shared system\n").unwrap();
        let result = read_configs(
            dir.path(),
            &[
                (dir.path().join("config"), dir.path().to_path_buf()),
                (dir.path().join("system"), dir.path().to_path_buf()),
            ],
        );
        assert_eq!(result.hosts, ["other", "shared", "spaces", "system"]);
    }

    #[test]
    fn limits_and_invalid_lines_leave_other_aliases_available() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("huge"), "x".repeat(MAX_FILE + 1)).unwrap();
        fs::write(
            dir.path().join("config"),
            "Host good\nInclude huge\nHost \"unclosed\nHost also-good\n",
        )
        .unwrap();
        let result = read_configs(
            dir.path(),
            &[(dir.path().join("config"), dir.path().to_path_buf())],
        );
        assert_eq!(result.hosts, ["also-good", "good"]);
        assert!(!result.warning.is_empty());
        let many = (0..MAX_HOSTS + 1)
            .map(|i| format!("Host host{i}\n"))
            .collect::<String>();
        let result = discover_text(&many);
        assert_eq!(result.hosts.len(), MAX_HOSTS);
        assert!(!result.warning.is_empty());
    }

    #[test]
    fn absent_configs_and_unrelated_directives_do_not_create_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_configs(
            dir.path(),
            &[(dir.path().join("missing"), dir.path().to_path_buf())],
        );
        assert!(result.hosts.is_empty());
        assert!(result.warning.is_empty());
        assert!(discover_text(
            "Host *\nIdentityFile ~/.ssh/id_ed25519\nProxyCommand echo anything\n"
        )
        .hosts
        .is_empty());
    }
}
