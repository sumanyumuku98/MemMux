//! Host-spec capture for the one-command reproducer (SUM-171 / P3).
//!
//! Records the machine a benchmark ran on — OS/arch, physical RAM, CPU model + logical core count,
//! and the harness version — into a [`HostSpec`] so a `report.md` header and a committed `host.json`
//! document the environment without any external crate or `unsafe`:
//!
//! * **Linux** reads `/proc/cpuinfo` (`model name`, core count) and `/proc/meminfo` (`MemTotal`)
//!   with pure parsers that are unit-tested against captured fixtures.
//! * **macOS** shells out to `sysctl -n hw.model machdep.cpu.brand_string hw.memsize hw.logicalcpu`
//!   via [`std::process::Command`] (best-effort; missing fields become `None`/`"unknown"`).
//! * **Other** platforms report the OS/arch and version only, leaving hardware fields unknown.

use serde::{Deserialize, Serialize};

/// A captured description of the host a benchmark ran on (SUM-171).
///
/// Hardware fields are best-effort: a field the platform could not resolve is `None` (numbers) or
/// rendered as `"unknown"` (strings), never fabricated. `os`/`arch`/`bench_version` are always
/// present.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSpec {
    /// Operating system (`std::env::consts::OS`, e.g. `"linux"`, `"macos"`).
    pub os: String,
    /// CPU architecture (`std::env::consts::ARCH`, e.g. `"x86_64"`, `"aarch64"`).
    pub arch: String,
    /// Physical RAM in bytes, or `None` when it could not be resolved.
    pub physical_ram_bytes: Option<u64>,
    /// CPU model string (e.g. `"Apple M2 Pro"`, `"Intel(R) Core(TM) i7-9750H"`), or `None`.
    pub cpu_model: Option<String>,
    /// Logical core count, or `None` when it could not be resolved.
    pub logical_cores: Option<usize>,
    /// The memmux/bench workspace version (`CARGO_PKG_VERSION`).
    pub bench_version: String,
}

impl HostSpec {
    /// Capture the current host's spec (best-effort, cross-platform, no external crates / `unsafe`).
    pub fn detect() -> Self {
        let mut spec = Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            physical_ram_bytes: None,
            cpu_model: None,
            logical_cores: None,
            bench_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        #[cfg(target_os = "linux")]
        {
            if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
                spec.cpu_model = cpu_model_from_cpuinfo(&cpuinfo);
                spec.logical_cores = logical_cores_from_cpuinfo(&cpuinfo);
            }
            if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
                spec.physical_ram_bytes = mem_total_bytes_from_meminfo(&meminfo);
            }
        }
        #[cfg(target_os = "macos")]
        {
            spec.apply_macos_sysctl();
        }
        spec
    }

    /// Fill CPU/RAM fields from `sysctl` on macOS (best-effort). Any failure leaves the field
    /// `None`/unknown rather than fabricating a value.
    #[cfg(target_os = "macos")]
    fn apply_macos_sysctl(&mut self) {
        // Model name (`hw.model`) is a coarse machine id; the CPU brand string is more useful when
        // present (Intel Macs), so prefer it and fall back to the model id.
        let brand = sysctl_string("machdep.cpu.brand_string");
        let model = sysctl_string("hw.model");
        self.cpu_model = brand.or(model);
        self.physical_ram_bytes = sysctl_u64("hw.memsize");
        self.logical_cores = sysctl_u64("hw.logicalcpu").map(|v| v as usize);
    }

    /// Human-readable physical-RAM string in GiB (one decimal), or `"unknown"` when unresolved.
    pub fn ram_display(&self) -> String {
        match self.physical_ram_bytes {
            Some(b) => format!("{:.1} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
            None => "unknown".to_string(),
        }
    }

    /// CPU model string or `"unknown"`.
    pub fn cpu_display(&self) -> String {
        self.cpu_model
            .clone()
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Logical-core count as a string, or `"unknown"`.
    pub fn cores_display(&self) -> String {
        self.logical_cores
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// Parse the CPU model from `/proc/cpuinfo` contents: the first `model name` value (Linux).
///
/// Returns `None` when there is no `model name` line (e.g. on some ARM kernels that use
/// `Hardware`/`Processor` instead), so the caller can render `"unknown"` rather than a wrong value.
pub fn cpu_model_from_cpuinfo(cpuinfo: &str) -> Option<String> {
    for line in cpuinfo.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim() == "model name" {
                let v = value.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Count logical cores from `/proc/cpuinfo`: the number of `processor` records (Linux).
///
/// Returns `None` when no `processor` line is present.
pub fn logical_cores_from_cpuinfo(cpuinfo: &str) -> Option<usize> {
    let count = cpuinfo
        .lines()
        .filter(|line| {
            line.split_once(':')
                .map(|(k, _)| k.trim() == "processor")
                .unwrap_or(false)
        })
        .count();
    if count == 0 {
        None
    } else {
        Some(count)
    }
}

/// Parse total physical RAM in bytes from `/proc/meminfo`: the `MemTotal:` value, which is in kB
/// (kibibytes) on Linux, converted to bytes.
///
/// Returns `None` when there is no parseable `MemTotal:` line.
pub fn mem_total_bytes_from_meminfo(meminfo: &str) -> Option<u64> {
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            // Format: `MemTotal:       16384000 kB`.
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Run `sysctl -n <name>` and return its trimmed stdout as a string, or `None` on any failure
/// (macOS). No `unsafe`, no libc: a plain [`std::process::Command`].
#[cfg(target_os = "macos")]
fn sysctl_string(name: &str) -> Option<String> {
    let output = std::process::Command::new("sysctl")
        .arg("-n")
        .arg(name)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Run `sysctl -n <name>` and parse its stdout as a `u64`, or `None` on any failure (macOS).
#[cfg(target_os = "macos")]
fn sysctl_u64(name: &str) -> Option<u64> {
    sysctl_string(name).and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed x86_64 `/proc/cpuinfo` fixture (two logical cores).
    const CPUINFO: &str = "\
processor\t: 0
vendor_id\t: GenuineIntel
cpu family\t: 6
model\t\t: 158
model name\t: Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz
stepping\t: 10

processor\t: 1
vendor_id\t: GenuineIntel
cpu family\t: 6
model\t\t: 158
model name\t: Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz
stepping\t: 10
";

    const MEMINFO: &str = "\
MemTotal:       16384000 kB
MemFree:         1234567 kB
MemAvailable:    8000000 kB
";

    #[test]
    fn parses_cpu_model_from_cpuinfo() {
        assert_eq!(
            cpu_model_from_cpuinfo(CPUINFO).as_deref(),
            Some("Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz")
        );
        assert_eq!(cpu_model_from_cpuinfo("no model here\n"), None);
        assert_eq!(cpu_model_from_cpuinfo(""), None);
    }

    #[test]
    fn counts_logical_cores_from_cpuinfo() {
        assert_eq!(logical_cores_from_cpuinfo(CPUINFO), Some(2));
        assert_eq!(logical_cores_from_cpuinfo("processor : 0\n"), Some(1));
        assert_eq!(logical_cores_from_cpuinfo("nothing\n"), None);
    }

    #[test]
    fn parses_mem_total_from_meminfo() {
        // 16384000 kB * 1024 = 16777216000 bytes.
        assert_eq!(mem_total_bytes_from_meminfo(MEMINFO), Some(16_777_216_000));
        assert_eq!(mem_total_bytes_from_meminfo("MemFree: 100 kB\n"), None);
        assert_eq!(mem_total_bytes_from_meminfo("MemTotal: garbage\n"), None);
        assert_eq!(mem_total_bytes_from_meminfo(""), None);
    }

    #[test]
    fn detect_always_fills_os_arch_and_version() {
        let spec = HostSpec::detect();
        assert!(!spec.os.is_empty());
        assert!(!spec.arch.is_empty());
        assert!(!spec.bench_version.is_empty());
        // Display helpers never panic and never return an empty string.
        assert!(!spec.ram_display().is_empty());
        assert!(!spec.cpu_display().is_empty());
        assert!(!spec.cores_display().is_empty());
    }

    #[test]
    fn ram_display_formats_gib() {
        let spec = HostSpec {
            physical_ram_bytes: Some(16 * 1024 * 1024 * 1024),
            ..Default::default()
        };
        assert_eq!(spec.ram_display(), "16.0 GiB");
        let unknown = HostSpec::default();
        assert_eq!(unknown.ram_display(), "unknown");
    }

    #[test]
    fn host_spec_json_round_trips() {
        let spec = HostSpec {
            os: "linux".into(),
            arch: "x86_64".into(),
            physical_ram_bytes: Some(16_777_216_000),
            cpu_model: Some("Intel(R) Core(TM) i7".into()),
            logical_cores: Some(12),
            bench_version: "0.4.0".into(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: HostSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }
}
