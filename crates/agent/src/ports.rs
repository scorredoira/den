//! TCP ports that processes started from the agent's terminals listen on, to
//! open them from the UI (over an SSH forward on a server).

use std::collections::HashMap;

use proto::PortInfo;

/// Listening ports reachable at the machine's loopback, each with the group
/// of the terminal whose shell started the process. `shells`: the pid of each
/// terminal's shell and its group. Only Linux for now (servers); elsewhere,
/// none.
pub fn listening(shells: &HashMap<u32, String>) -> Vec<PortInfo> {
    #[cfg(target_os = "linux")]
    {
        linux::listening(shells)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = shells;
        Vec::new()
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
