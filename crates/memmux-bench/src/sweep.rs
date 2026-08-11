//! N-sweep list parsing for the agent-count sweep (SUM-169 / P3).
//!
//! The `run`/`paper` subcommands accept an `--agents-sweep` comma list (e.g. `"1,5,10,20"`) that
//! overrides the single `--agents` count and runs each (launcher × scenario) cell once per N. This
//! is the pure parser for that list: it splits, parses, deduplicates, and sorts the counts so the
//! footprint-vs-N figure and the per-N JSONL filenames see a stable ascending set.

/// Parse a comma-separated agent-count sweep list into a sorted, deduplicated `Vec<usize>`.
///
/// Whitespace around each element is ignored. Every element must be a positive integer (`>= 1`); a
/// non-numeric element or a `0` is rejected with a message naming the offending token, and an empty
/// list (after trimming) is likewise rejected — a sweep must contain at least one N. The result is
/// sorted ascending with duplicates removed, so `"20, 5, 5, 1"` parses to `[1, 5, 20]`.
///
/// # Examples
///
/// ```
/// use memmux_bench::sweep::parse_agents_sweep;
/// assert_eq!(parse_agents_sweep("1,5,10,20").unwrap(), vec![1, 5, 10, 20]);
/// assert_eq!(parse_agents_sweep("20, 5, 5, 1").unwrap(), vec![1, 5, 20]);
/// assert!(parse_agents_sweep("1,two,3").is_err());
/// assert!(parse_agents_sweep("0,1").is_err());
/// assert!(parse_agents_sweep("  ").is_err());
/// ```
pub fn parse_agents_sweep(list: &str) -> Result<Vec<usize>, String> {
    let mut out: Vec<usize> = Vec::new();
    for token in list.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            // Skip empties from trailing commas etc.; a fully-empty list is caught below.
            continue;
        }
        let n: usize = trimmed
            .parse()
            .map_err(|_| format!("invalid agent count '{trimmed}' in sweep list"))?;
        if n == 0 {
            return Err(format!("agent count must be >= 1, got '{trimmed}'"));
        }
        out.push(n);
    }
    if out.is_empty() {
        return Err("agents-sweep list is empty".to_string());
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_canonical_sweep() {
        assert_eq!(parse_agents_sweep("1,5,10,20").unwrap(), vec![1, 5, 10, 20]);
    }

    #[test]
    fn dedups_and_sorts() {
        assert_eq!(parse_agents_sweep("20, 5, 5, 1").unwrap(), vec![1, 5, 20]);
        assert_eq!(parse_agents_sweep("3,3,3").unwrap(), vec![3]);
    }

    #[test]
    fn tolerates_whitespace_and_trailing_commas() {
        assert_eq!(parse_agents_sweep(" 1 , 2 ,").unwrap(), vec![1, 2]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_agents_sweep("1,two,3").is_err());
        assert!(parse_agents_sweep("abc").is_err());
        assert!(parse_agents_sweep("1.5").is_err());
    }

    #[test]
    fn rejects_zero_and_empty() {
        assert!(parse_agents_sweep("0").is_err());
        assert!(parse_agents_sweep("0,1").is_err());
        assert!(parse_agents_sweep("").is_err());
        assert!(parse_agents_sweep("  ").is_err());
        assert!(parse_agents_sweep(",,").is_err());
    }

    #[test]
    fn single_value_is_a_valid_sweep() {
        assert_eq!(parse_agents_sweep("7").unwrap(), vec![7]);
    }
}
