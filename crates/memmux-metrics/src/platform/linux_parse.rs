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
    /// Minor faults (`stat` field 10) — cumulative; `None` if the field was absent/unparsable.
    pub minflt: Option<u64>,
    /// Major faults (`stat` field 12) — cumulative; the I/O-backed thrashing signal.
    pub majflt: Option<u64>,
}

/// Memory fields extracted from a `/proc/<pid>/smaps_rollup` file, in **bytes**.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SmapsRollup {
    /// Proportional set size (`Pss:`).
    pub pss_bytes: Option<u64>,
    /// Unique set size = `Private_Clean` + `Private_Dirty`.
    pub uss_bytes: Option<u64>,
    /// Swapped-out bytes (`Swap:`).
    pub swap_bytes: Option<u64>,
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
    // Fields after `comm`, 0-indexed: 0=state, 1=ppid, …, 7=minflt, 9=majflt (these are `stat`
    // fields 3, 4, 10, 12 respectively).
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let state = fields.first()?.chars().next()?;
    let ppid: Pid = fields.get(1)?.parse().ok()?;
    let minflt = fields.get(7).and_then(|f| f.parse::<u64>().ok());
    let majflt = fields.get(9).and_then(|f| f.parse::<u64>().ok());
    Some(StatInfo {
        pid,
        comm,
        state,
        ppid,
        minflt,
        majflt,
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

/// Parse PSS, USS (`Private_Clean+Private_Dirty`), and `Swap:` from a `/proc/<pid>/smaps_rollup`
/// file, all in **bytes**. Each field is `None` if its line is absent (so a caller can tell
/// "not reported" from "zero"). Values are summed defensively in case a full `smaps` is passed.
pub fn parse_smaps_rollup(content: &str) -> SmapsRollup {
    let mut pss_kb: Option<u64> = None;
    let mut private_kb: Option<u64> = None;
    let mut swap_kb: Option<u64> = None;
    let add = |slot: &mut Option<u64>, rest: &str| {
        if let Ok(kb) = rest.trim().trim_end_matches("kB").trim().parse::<u64>() {
            *slot = Some(slot.unwrap_or(0) + kb);
        }
    };
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("Pss:") {
            add(&mut pss_kb, rest);
        } else if let Some(rest) = line.strip_prefix("Private_Clean:") {
            add(&mut private_kb, rest);
        } else if let Some(rest) = line.strip_prefix("Private_Dirty:") {
            add(&mut private_kb, rest);
        } else if let Some(rest) = line.strip_prefix("Swap:") {
            // Guard against `SwapPss:` also starting with "Swap" — strip_prefix("Swap:") already
            // requires the colon, so `SwapPss:` does not match. Good.
            add(&mut swap_kb, rest);
        }
    }
    SmapsRollup {
        pss_bytes: pss_kb.map(|kb| kb * 1024),
        uss_bytes: private_kb.map(|kb| kb * 1024),
        swap_bytes: swap_kb.map(|kb| kb * 1024),
    }
}

/// Parse currently-used swap in **bytes** from `/proc/meminfo` (`SwapTotal - SwapFree`).
///
/// Both fields are in kiB. Returns `None` only if either line is absent; a system with swap
/// disabled reports both as 0 and yields `Some(0)`. Used as the pressure-ladder swap signal
/// (SUM-48): a rising value across ticks is a leading indicator of thrashing.
pub fn parse_swap_used_meminfo(content: &str) -> Option<u64> {
    let mut total_kib: Option<u64> = None;
    let mut free_kib: Option<u64> = None;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("SwapTotal:") {
            total_kib = rest.trim().trim_end_matches("kB").trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("SwapFree:") {
            free_kib = rest.trim().trim_end_matches("kB").trim().parse().ok();
        }
    }
    Some(total_kib?.saturating_sub(free_kib?) * 1024)
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
    fn parse_stat_extracts_page_faults() {
        // stat field layout after comm: state(3) ppid(4) pgrp(5) session(6) tty(7) tpgid(8)
        // flags(9) minflt(10) cminflt(11) majflt(12) cmajflt(13) ...
        let line = "1234 (bash) S 1000 1234 1234 0 -1 4194304 111 0 22 0 0 0";
        let info = parse_stat(line).unwrap();
        assert_eq!(info.minflt, Some(111));
        assert_eq!(info.majflt, Some(22));
    }

    #[test]
    fn parse_smaps_rollup_pss_uss_swap() {
        let rollup = "\
Rss:               20000 kB
Pss:               12000 kB
Private_Clean:      3000 kB
Private_Dirty:      5000 kB
Swap:                800 kB
SwapPss:             400 kB
";
        let r = parse_smaps_rollup(rollup);
        assert_eq!(r.pss_bytes, Some(12000 * 1024));
        // USS = Private_Clean + Private_Dirty = 8000 kB.
        assert_eq!(r.uss_bytes, Some(8000 * 1024));
        // `Swap:` only (not `SwapPss:`).
        assert_eq!(r.swap_bytes, Some(800 * 1024));
    }

    #[test]
    fn parse_smaps_rollup_absent_fields_are_none() {
        let r = parse_smaps_rollup("Rss: 100 kB\n");
        assert_eq!(r, SmapsRollup::default());
    }

    #[test]
    fn parse_swap_used_computes_total_minus_free() {
        let meminfo = "\
MemTotal:       32000000 kB
MemFree:         1000000 kB
SwapTotal:       8000000 kB
SwapFree:        6000000 kB
";
        // (8_000_000 - 6_000_000) kiB * 1024 = 2_000_000 * 1024 bytes.
        assert_eq!(parse_swap_used_meminfo(meminfo), Some(2_000_000 * 1024));
    }

    #[test]
    fn parse_swap_used_zero_when_disabled_and_none_when_absent() {
        let disabled = "SwapTotal:             0 kB\nSwapFree:              0 kB\n";
        assert_eq!(parse_swap_used_meminfo(disabled), Some(0));
        assert_eq!(parse_swap_used_meminfo("MemTotal: 32000000 kB\n"), None);
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
