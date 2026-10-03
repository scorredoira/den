//! TCP ports that processes started from the agent's terminals listen on, to
//! open them from the UI (over an SSH forward on a server).

use std::collections::HashMap;

use proto::PortInfo;

/// Listening ports reachable at the machine's loopback, each with the group
/// of the terminal whose shell started the process. `shells`: the pid of each
/// terminal's shell and its group. Linux and macOS; elsewhere, none.
pub fn listening(shells: &HashMap<u32, String>) -> Vec<PortInfo> {
    #[cfg(target_os = "linux")]
    {
        linux::listening(shells)
    }
    #[cfg(target_os = "macos")]
    {
        macos::listening(shells)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = shells;
        Vec::new()
    }
}

/// Without `/proc`: `ps` gives the tree of processes and `lsof` the sockets
/// of those started from a terminal.
#[cfg(any(target_os = "macos", test))]
mod macos {
    use std::collections::HashMap;

    #[cfg(target_os = "macos")]
    use proto::PortInfo;

    #[cfg(target_os = "macos")]
    pub fn listening(shells: &HashMap<u32, String>) -> Vec<PortInfo> {
        let Some(tree) = output("ps", &["-axo", "pid=,ppid="]) else {
            return Vec::new();
        };
        let parents = parse_tree(&tree);
        let groups: HashMap<u32, String> = parents
            .keys()
            .filter_map(|&pid| Some((pid, shell_group(pid, &parents, shells)?)))
            .collect();
        if groups.is_empty() {
            return Vec::new();
        }
        let pids: Vec<String> = groups.keys().map(u32::to_string).collect();
        let Some(sockets) = output("lsof", &["-nP", "-a", "-p", &pids.join(","), "-iTCP", "-sTCP:LISTEN", "-Fpcn"]) else {
            return Vec::new();
        };
        let mut ports: Vec<PortInfo> = Vec::new();
        for (pid, process, port) in parse_lsof(&sockets) {
            if ports.iter().any(|known| known.port == port) {
                continue;
            }
            if let Some(group) = groups.get(&pid) {
                ports.push(PortInfo { port, group: group.clone(), process });
            }
        }
        ports.sort_by_key(|info| info.port);
        ports
    }

    /// Its standard output, also when it fails: `lsof` does when one of the
    /// processes has nothing to list.
    #[cfg(target_os = "macos")]
    fn output(program: &str, args: &[&str]) -> Option<String> {
        let output = std::process::Command::new(program).args(args).output().ok()?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// The group of the terminal whose shell is `pid` or one of its ancestors.
    fn shell_group(mut pid: u32, parents: &HashMap<u32, u32>, shells: &HashMap<u32, String>) -> Option<String> {
        for _ in 0..64 {
            if let Some(group) = shells.get(&pid) {
                return Some(group.clone());
            }
            pid = *parents.get(&pid)?;
            if pid <= 1 {
                return None;
            }
        }
        None
    }

    /// `ps -o pid=,ppid=`: pid → parent.
    pub(super) fn parse_tree(text: &str) -> HashMap<u32, u32> {
        text.lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            })
            .collect()
    }

    /// `lsof -Fpcn` of listening sockets: (pid, command, port) of those
    /// reachable at loopback (bound to it or to every address).
    pub(super) fn parse_lsof(text: &str) -> Vec<(u32, String, u16)> {
        let mut sockets = Vec::new();
        let (mut pid, mut command) = (0, String::new());
        for line in text.lines() {
            let (field, value) = line.split_at(line.len().min(1));
            match field {
                "p" => pid = value.parse().unwrap_or(0),
                "c" => command = value.to_string(),
                "n" => {
                    let Some((host, port)) = value.rsplit_once(':') else {
                        continue;
                    };
                    let host = host.trim_start_matches('[').trim_end_matches(']');
                    let loopback = host == "*"
                        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| {
                            ip.is_unspecified()
                                || ip.is_loopback()
                                || matches!(ip, std::net::IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some_and(|v4| v4.is_unspecified() || v4.is_loopback()))
                        });
                    if loopback && let Ok(port) = port.parse() {
                        sockets.push((pid, command.clone(), port));
                    }
                }
                _ => {}
            }
        }
        sockets
    }
}

#[cfg(any(target_os = "linux", test))]
mod linux {
    use std::{collections::HashMap, net::Ipv6Addr};

    #[cfg(target_os = "linux")]
    use proto::PortInfo;

    /// `LISTEN` in `/proc/net/tcp`.
    const LISTEN: &str = "0A";

    #[cfg(target_os = "linux")]
    pub fn listening(shells: &HashMap<u32, String>) -> Vec<PortInfo> {
        let mut sockets = HashMap::new();
        for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
            if let Ok(table) = std::fs::read_to_string(file) {
                sockets.extend(parse_table(&table));
            }
        }
        if sockets.is_empty() {
            return Vec::new();
        }
        let mut ports: Vec<PortInfo> = Vec::new();
        for (pid, inode) in socket_owners() {
            let Some(&port) = sockets.get(&inode) else {
                continue;
            };
            let Some(group) = shell_group(pid, shells) else {
                continue;
            };
            if ports.iter().any(|known| known.port == port) {
                continue;
            }
            let process = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|name| name.trim().to_string())
                .unwrap_or_default();
            ports.push(PortInfo { port, group, process });
        }
        ports.sort_by_key(|info| info.port);
        ports
    }

    /// `(pid, socket inode)` of every socket of the processes we can see
    /// (those of our user).
    #[cfg(target_os = "linux")]
    fn socket_owners() -> Vec<(u32, u64)> {
        let mut owners = Vec::new();
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return owners;
        };
        for entry in procs.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let Ok(target) = std::fs::read_link(fd.path()) else {
                    continue;
                };
                if let Some(inode) = target
                    .to_str()
                    .and_then(|target| target.strip_prefix("socket:["))
                    .and_then(|rest| rest.strip_suffix(']'))
                    .and_then(|inode| inode.parse().ok())
                {
                    owners.push((pid, inode));
                }
            }
        }
        owners
    }

    /// The group of the terminal whose shell is `pid` or one of its ancestors.
    #[cfg(target_os = "linux")]
    fn shell_group(mut pid: u32, shells: &HashMap<u32, String>) -> Option<String> {
        for _ in 0..64 {
            if let Some(group) = shells.get(&pid) {
                return Some(group.clone());
            }
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            pid = parent_pid(&stat)?;
            if pid <= 1 {
                return None;
            }
        }
        None
    }

    /// The parent in a `/proc/<pid>/stat` line: its name, in parentheses, may
    /// contain spaces and parentheses, so it's counted from the last `)`.
    pub(super) fn parent_pid(stat: &str) -> Option<u32> {
        stat[stat.rfind(')')? + 1..].split_whitespace().nth(1)?.parse().ok()
    }

    /// Listening sockets of a `/proc/net/tcp{,6}` table reachable at loopback
    /// (bound to it or to every address): inode → port.
    pub(super) fn parse_table(table: &str) -> HashMap<u64, u16> {
        let mut sockets = HashMap::new();
        for line in table.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (Some(local), Some(&state), Some(inode)) = (fields.get(1), fields.get(3), fields.get(9)) else {
                continue;
            };
            let Some((address, port)) = local.split_once(':') else {
                continue;
            };
            let (Ok(port), Ok(inode)) = (u16::from_str_radix(port, 16), inode.parse::<u64>()) else {
                continue;
            };
            if state == LISTEN && inode != 0 && reaches_loopback(address) {
                sockets.insert(inode, port);
            }
        }
        sockets
    }

    /// An address from the table (hex, in 32-bit words in the kernel's byte
    /// order, little endian on the servers we support).
    fn reaches_loopback(hex: &str) -> bool {
        let words: Option<Vec<u32>> = (0..hex.len() / 8)
            .map(|i| u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok().map(u32::swap_bytes))
            .collect();
        match words.as_deref() {
            Some([v4]) => *v4 == 0 || v4 >> 24 == 127,
            Some([a, b, c, d]) => {
                let ip = Ipv6Addr::new(
                    (a >> 16) as u16, *a as u16, (b >> 16) as u16, *b as u16,
                    (c >> 16) as u16, *c as u16, (d >> 16) as u16, *d as u16,
                );
                ip.is_unspecified()
                    || ip.is_loopback()
                    || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_unspecified() || v4.is_loopback())
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::linux::{parent_pid, parse_table};
    use super::macos::{parse_lsof, parse_tree};

    #[test]
    fn parses_lsof() {
        let text = "p7295\ncscl\nf4\nn127.0.0.1:4444\nf2954\nn*:9092\np812\ncnode\nf20\nn[::1]:3000\nf21\nn192.168.1.4:5000\nf22\nn[::]:8080\n";
        assert_eq!(
            parse_lsof(text),
            vec![
                (7295, "scl".to_string(), 4444),
                (7295, "scl".to_string(), 9092),
                (812, "node".to_string(), 3000),
                (812, "node".to_string(), 8080),
            ]
        );
    }

    #[test]
    fn parses_process_tree() {
        let tree = parse_tree("    1     0\n 7287 56282\n 7295  7287\n");
        assert_eq!(tree[&7295], 7287);
        assert_eq!(tree[&7287], 56282);
        assert_eq!(tree.len(), 3);
    }

    #[test]
    fn parses_listening_sockets() {
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0CEA 00000000:0000 0A 00000000:00000000 00:00000000 00000000   122        0 15845 1
   1: 00000000:2008 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 17085437 1
   2: 37A66064:AC52 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 101201907 1
   3: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 555 1";
        let sockets = parse_table(v4);
        assert_eq!(sockets.len(), 2);
        assert_eq!(sockets[&15845], 3306);
        assert_eq!(sockets[&17085437], 8200);

        let v6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 188311232 1
   1: 00000000000000000000000001000000:0277 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 184691709 1
   2: 5C117AFD0000E0A100000000D6A601DA:DFAB 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 101201909 1";
        let sockets = parse_table(v6);
        assert_eq!(sockets.len(), 2);
        assert_eq!(sockets[&188311232], 8080);
        assert_eq!(sockets[&184691709], 631);
    }

    #[test]
    fn reads_parent_pid() {
        assert_eq!(parent_pid("2255059 (sim) S 1937967 2255059 1937967 0"), Some(1937967));
        assert_eq!(parent_pid("42 (a) b (c) R 7 42 42"), Some(7));
    }
}
