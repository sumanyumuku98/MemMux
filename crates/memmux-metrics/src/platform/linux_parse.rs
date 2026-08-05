//! Pure parsers for the Linux `/proc` files MemMux samples.
//!
//! These are intentionally free of any filesystem access so they can be exhaustively unit
//! tested on any host (including macOS CI). The Linux sampler in `linux.rs` reads the files
//! and delegates here (SUM-27).

use memmux_core::ids::Pid;

/// Fields extracted from `/proc/<pid>/stat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatInfo {
    /// The process id.
    pub pid: Pid,
    /// The command name (`comm`), without surrounding parentheses.
    pub comm: String,
    /// The process state character (`R`, `S`, `D`, `Z` for zombie, `T`, …).
    pub state: char,
    /// The parent process id.
    pub ppid: Pid,
}

impl StatInfo {
    /// Whether the process is a zombie (defunct, pending reap) — holds no memory and is
    /// effectively gone for MemMux accounting/termination purposes.
    pub fn is_zombie(&self) -> bool {
        self.state == 'Z'
    }
}

/// Parse the parts of `/proc/<pid>/stat` MemMux needs: pid, `comm`, and ppid.
///
/// `comm` is delimited by the first `(` and the *last* `)` because it may itself contain
/// spaces or parentheses (e.g. `(a (weird) name)`). The fields after the closing paren are
/// space-separated: index 0 is `state`, index 1 is `ppid`.
pub fn parse_stat(content: &str) -> Option<StatInfo> {
    let open = content.find('(')?;
    let close = content.rfind(')')?;
    if close < open {
        return None;
    }
    let pid: Pid = content[..open].trim().parse().ok()?;
    let comm = content[open + 1..close].to_string();
    let rest = content[close + 1..].trim();
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid: Pid = fields.next()?.parse().ok()?;
    Some(StatInfo {
        pid,
        comm,
        state,
        ppid,
    })
}

/// Parse a `SIZE:` style field (in kB) from a `/proc/<pid>/status` file, returning **bytes**.
///
/// Example line: `VmRSS:\t   12345 kB`.
pub fn parse_status_kb_field(content: &str, field: &str) -> Option<u64> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Parse the `Pss:` total from a `/proc/<pid>/smaps_rollup` file, returning **bytes**.
///
/// `smaps_rollup` has a single `Pss:` line, but we sum defensively in case a caller passes a
/// full `smaps` file instead.
pub fn parse_smaps_rollup_pss(content: &str) -> Option<u64> {
    let mut total_kb: u64 = 0;
    let mut found = false;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("Pss:") {
            if let Ok(kb) = rest.trim().trim_end_matches("kB").trim().parse::<u64>() {
                total_kb += kb;
                found = true;
            }
        }
    }
    found.then_some(total_kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stat_simple() {
        let line = "1234 (bash) S 1000 1234 1234 0 -1 4194304 100 0 0 0";
        let info = parse_stat(line).unwrap();
        assert_eq!(info.pid, 1234);
        assert_eq!(info.comm, "bash");
        assert_eq!(info.ppid, 1000);
        assert_eq!(info.state, 'S');
        assert!(!info.is_zombie());
    }

    #[test]
    fn parse_stat_detects_zombie() {
        let line = "999 (defunct) Z 1 999 999 0 -1 0 0";
        let info = parse_stat(line).unwrap();
        assert_eq!(info.state, 'Z');
        assert!(info.is_zombie());
    }

    #[test]
    fn parse_stat_comm_with_spaces_and_parens() {
        let line = "42 (a (weird) name) R 7 42 42 0 -1 0 0";
        let info = parse_stat(line).unwrap();
        assert_eq!(info.pid, 42);
        assert_eq!(info.comm, "a (weird) name");
        assert_eq!(info.ppid, 7);
    }

    #[test]
    fn parse_stat_rejects_garbage() {
        assert!(parse_stat("not a stat line").is_none());
        assert!(parse_stat("").is_none());
    }

    #[test]
    fn parse_status_vmrss_to_bytes() {
        let status = "Name:\tbash\nState:\tS (sleeping)\nVmRSS:\t   2048 kB\nThreads:\t1\n";
        assert_eq!(parse_status_kb_field(status, "VmRSS:"), Some(2048 * 1024));
        assert_eq!(parse_status_kb_field(status, "VmSwap:"), None);
    }

    #[test]
    fn parse_smaps_rollup_single_pss() {
        let rollup = "55f0-55f9 ---p 00000000 00:00 0 [rollup]\nRss:  4096 kB\nPss:  1536 kB\n";
        assert_eq!(parse_smaps_rollup_pss(rollup), Some(1536 * 1024));
    }

    #[test]
    fn parse_smaps_rollup_sums_multiple_pss() {
        let smaps = "Pss:  100 kB\nother\nPss:  200 kB\n";
        assert_eq!(parse_smaps_rollup_pss(smaps), Some(300 * 1024));
        assert_eq!(parse_smaps_rollup_pss("no pss here"), None);
    }
}
