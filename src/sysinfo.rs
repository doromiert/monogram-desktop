//! System information for the `/neofetch` client command: board or laptop model, processor, memory,
//! total disk, uptime, distribution and this client's version.
//!
//! Everything is read from `/proc`, `/sys` and `/etc/os-release` (no subprocesses). By design it
//! never includes the user name or the host name, so the output is safe to paste into a bug report.
//! `collect_from` takes the filesystem root so tests can run against a fake tree.
use std::{collections::BTreeSet, fs, path::Path};

#[derive(Debug, PartialEq, Eq)]
pub struct Info {
    /// "Model" for a laptop, "Motherboard" for anything else.
    pub machine: (&'static str, String),
    pub cpu: String,
    pub memory_total: u64,
    pub memory_used: u64,
    pub disk_total: u64,
    pub uptime_seconds: u64,
    pub os: String,
    pub client: String,
}

fn read(root: &Path, relative: &str) -> Option<String> {
    let text = fs::read_to_string(root.join(relative)).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Firmware often leaves placeholders instead of real names.
fn real(value: Option<String>) -> Option<String> {
    value.filter(|v| {
        let lower = v.to_lowercase();
        !["none", "default string", "not specified", "system version", "system product name",
            "to be filled by o.e.m.", "type1productconfigid", "n/a", "unknown"]
            .iter()
            .any(|junk| lower == *junk)
    })
}

/// "LENOVO" -> "Lenovo"; short or mixed-case names ("HP", "ASUSTeK") are left alone.
fn vendor_case(vendor: &str) -> String {
    if vendor.len() > 3 && vendor.chars().all(|c| c.is_ascii_uppercase()) {
        let mut chars = vendor.chars();
        chars.next().map(|first| first.to_string() + &chars.as_str().to_lowercase()).unwrap_or_default()
    } else {
        vendor.to_owned()
    }
}

fn machine(root: &Path) -> (&'static str, String) {
    let id = |name: &str| real(read(root, &format!("sys/class/dmi/id/{name}")));
    // DMI chassis types: portable, laptop, notebook, sub-notebook, tablet, convertible, detachable.
    let laptop = read(root, "sys/class/dmi/id/chassis_type")
        .and_then(|value| value.parse::<u32>().ok())
        .is_some_and(|kind| matches!(kind, 8 | 9 | 10 | 14 | 30 | 31 | 32));
    let vendor = id("sys_vendor").map(|v| vendor_case(&v));
    let product = id("product_name");
    if laptop {
        // Lenovo keeps the marketing name ("ThinkPad L13 Yoga") in product_version and the
        // machine type ("20R50007UK") in product_name; show both when they differ.
        let friendly = id("product_version").filter(|v| Some(v) != product.as_ref());
        let name = match (friendly, product) {
            (Some(friendly), Some(code)) => format!("{friendly} ({code})"),
            (Some(name), None) | (None, Some(name)) => name,
            (None, None) => "Unknown laptop".into(),
        };
        let full = match vendor {
            Some(vendor) if !name.to_lowercase().starts_with(&vendor.to_lowercase()) => format!("{vendor} {name}"),
            _ => name,
        };
        return ("Model", full);
    }
    let board = id("board_name");
    let name = match (id("board_vendor").map(|v| vendor_case(&v)), board.or(product)) {
        (Some(vendor), Some(name)) => format!("{vendor} {name}"),
        (None, Some(name)) => name,
        (Some(vendor), None) => vendor,
        (None, None) => "Unknown".into(),
    };
    ("Motherboard", name)
}

/// "Intel(R) Core(TM) i5-10210U CPU @ 1.60GHz" -> "Intel Core i5-10210U @ 1.60 GHz", plus the
/// core and thread counts.
fn cpu(root: &Path) -> String {
    let text = read(root, "proc/cpuinfo").unwrap_or_default();
    let name = text
        .lines()
        .find_map(|line| line.strip_prefix("model name").and_then(|rest| rest.split_once(':')))
        .map(|(_, value)| value.trim())
        .or_else(|| {
            text.lines()
                .find_map(|line| line.strip_prefix("Hardware").and_then(|rest| rest.split_once(':')).map(|(_, v)| v.trim()))
        })
        .unwrap_or("Unknown processor");
    let mut name = name.replace("(R)", "").replace("(TM)", "").replace(" CPU", "");
    for unit in ["GHz", "MHz"] {
        name = name.replace(&format!("{unit}"), &format!(" {unit}")).replace(&format!("  {unit}"), &format!(" {unit}"));
    }
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = |line: &str, key: &str| line.strip_prefix(key).and_then(|rest| rest.split_once(':')).map(|(_, v)| v.trim().to_owned());
    let (mut threads, mut cores, mut package) = (0usize, BTreeSet::new(), String::new());
    for line in text.lines() {
        if value(line, "processor").is_some() {
            threads += 1;
        }
        if let Some(id) = value(line, "physical id") {
            package = id;
        }
        if let Some(core) = value(line, "core id") {
            cores.insert((package.clone(), core));
        }
    }
    let cores = if cores.is_empty() { threads } else { cores.len() };
    match (cores, threads) {
        (0, _) => name,
        (c, t) if c == t => format!("{name} ({t} threads)"),
        (c, t) => format!("{name} ({c} cores / {t} threads)"),
    }
}

fn memory(root: &Path) -> (u64, u64) {
    let text = read(root, "proc/meminfo").unwrap_or_default();
    let kib = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
            .map_or(0, |value| value * 1024)
    };
    let total = kib("MemTotal");
    // "Used" the way `free` reports it: what applications hold, not reclaimable cache.
    (total, total.saturating_sub(kib("MemAvailable")))
}

/// Total size of the physical, non-removable drives. Loop, RAM and device-mapper devices are not
/// drives and are skipped, so nothing is counted twice.
fn disks(root: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(root.join("sys/block")) else { return 0 };
    entries
        .flatten()
        .filter(|entry| entry.path().join("device").exists())
        .filter(|entry| read(root, &format!("sys/block/{}/removable", entry.file_name().to_string_lossy())).as_deref() != Some("1"))
        .filter_map(|entry| {
            let sectors = read(root, &format!("sys/block/{}/size", entry.file_name().to_string_lossy()))?;
            sectors.parse::<u64>().ok()?.checked_mul(512)
        })
        .fold(0u64, u64::saturating_add)
}

fn distribution(root: &Path) -> String {
    let text = read(root, "etc/os-release").or_else(|| read(root, "usr/lib/os-release")).unwrap_or_default();
    let field = |key: &str| {
        text.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.strip_prefix('=')?;
            Some(value.trim().trim_matches(|c| c == '"' || c == '\'').to_owned())
        })
    };
    field("PRETTY_NAME").or_else(|| field("NAME")).filter(|v| !v.is_empty()).unwrap_or_else(|| "Linux".into())
}

/// This client's version, and the git commit it was built from when that was known at build time.
pub fn client() -> String {
    client_from(env!("CARGO_PKG_VERSION"), option_env!("MONOGRAM_GIT_COMMIT"), option_env!("MONOGRAM_GIT_DIRTY"))
}
fn client_from(version: &str, commit: Option<&str>, dirty: Option<&str>) -> String {
    match commit.filter(|c| !c.is_empty()) {
        Some(commit) if dirty == Some("1") => format!("{version} ({commit}, uncommitted changes)"),
        Some(commit) => format!("{version} ({commit})"),
        None => version.to_owned(),
    }
}

pub fn collect() -> Info {
    collect_from(Path::new("/"), client())
}
pub fn collect_from(root: &Path, client: String) -> Info {
    let (memory_total, memory_used) = memory(root);
    let uptime_seconds = read(root, "proc/uptime")
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0, |seconds| seconds as u64);
    Info {
        machine: machine(root),
        cpu: cpu(root),
        memory_total,
        memory_used,
        disk_total: disks(root),
        uptime_seconds,
        os: distribution(root),
        client,
    }
}

fn size(bytes: u64) -> String {
    const GIB: f64 = 1024. * 1024. * 1024.;
    let gib = bytes as f64 / GIB;
    if gib >= 1024. {
        format!("{:.2} TiB", gib / 1024.)
    } else if gib >= 1. {
        format!("{gib:.2} GiB")
    } else {
        format!("{:.0} MiB", bytes as f64 / (1024. * 1024.))
    }
}

fn uptime(seconds: u64) -> String {
    let plural = |n: u64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    let parts: Vec<String> = [(seconds / 86_400, "day"), (seconds / 3600 % 24, "hour"), (seconds / 60 % 60, "minute")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| plural(n, unit))
        .collect();
    if parts.is_empty() { "less than a minute".into() } else { parts.join(", ") }
}

impl Info {
    /// The report, one `Label      value` line each, aligned like neofetch (no logo yet).
    pub fn render(&self) -> String {
        let percent = if self.memory_total == 0 { 0 } else { (self.memory_used * 100 + self.memory_total / 2) / self.memory_total };
        let rows = [
            (self.machine.0, self.machine.1.clone()),
            ("CPU", self.cpu.clone()),
            ("Memory", format!("{} / {} ({percent}%)", size(self.memory_used), size(self.memory_total))),
            ("Disk", format!("{} total", size(self.disk_total))),
            ("Uptime", uptime(self.uptime_seconds)),
            ("OS", self.os.clone()),
            ("Monogram", self.client.clone()),
        ];
        let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0) + 2;
        rows.iter().map(|(label, value)| format!("{label:<width$}{value}")).collect::<Vec<_>>().join("\n")
    }
}

/// Whether composer text is the `/neofetch` client command (and nothing else).
pub fn is_neofetch(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("/neofetch")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake root with just the files each test needs.
    struct Tree(std::path::PathBuf);
    impl Tree {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("sysinfo-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn file(&self, relative: &str, contents: &str) -> &Self {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
            self
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const CPUINFO: &str = "processor\t: 0\nmodel name\t: Intel(R) Core(TM) i5-10210U CPU @ 1.60GHz\nphysical id\t: 0\ncore id\t\t: 0\n\nprocessor\t: 1\nmodel name\t: Intel(R) Core(TM) i5-10210U CPU @ 1.60GHz\nphysical id\t: 0\ncore id\t\t: 0\n\nprocessor\t: 2\nmodel name\t: Intel(R) Core(TM) i5-10210U CPU @ 1.60GHz\nphysical id\t: 0\ncore id\t\t: 1\n\nprocessor\t: 3\nmodel name\t: Intel(R) Core(TM) i5-10210U CPU @ 1.60GHz\nphysical id\t: 0\ncore id\t\t: 1\n";

    fn laptop() -> Tree {
        let tree = Tree::new();
        tree.file("sys/class/dmi/id/sys_vendor", "LENOVO\n")
            .file("sys/class/dmi/id/product_name", "20R50007UK\n")
            .file("sys/class/dmi/id/product_version", "ThinkPad L13 Yoga\n")
            .file("sys/class/dmi/id/chassis_type", "31\n")
            .file("sys/class/dmi/id/board_vendor", "LENOVO\n")
            .file("sys/class/dmi/id/board_name", "20R50007UK\n")
            .file("proc/cpuinfo", CPUINFO)
            .file("proc/meminfo", "MemTotal:        7702148 kB\nMemFree:          100 kB\nMemAvailable:    4807000 kB\n")
            .file("proc/uptime", "193740.55 1500000.00\n")
            .file("etc/os-release", "NAME=\"CachyOS Linux\"\nPRETTY_NAME=\"CachyOS\"\nID=cachyos\n")
            .file("sys/block/nvme0n1/size", "515521536\n")
            .file("sys/block/nvme0n1/removable", "0\n")
            .file("sys/block/nvme0n1/device/model", "x\n")
            .file("sys/block/sda/size", "1000000\n")
            .file("sys/block/sda/removable", "1\n")
            .file("sys/block/sda/device/model", "usb stick\n")
            .file("sys/block/loop0/size", "999999999\n")
            .file("sys/block/zram0/size", "999999999\n");
        tree
    }

    #[test]
    fn values_are_parsed_from_the_files_they_come_from() {
        let tree = laptop();
        let info = collect_from(&tree.0, "x".into());
        // MemTotal and MemAvailable are in kB; used is total minus available, like `free`.
        assert_eq!(info.memory_total, 7_702_148 * 1024);
        assert_eq!(info.memory_used, (7_702_148 - 4_807_000) * 1024);
        // 193740.55 s is 2 days, 5 hours, 49 minutes.
        assert_eq!(info.uptime_seconds, 193_740);
        assert_eq!(uptime(info.uptime_seconds), "2 days, 5 hours, 49 minutes");
        assert_eq!(info.disk_total, 515_521_536 * 512, "only the fixed NVMe drive counts");
        assert_eq!((info.os.as_str(), info.machine.0), ("CachyOS", "Model"));
        assert_eq!(info.machine.1, "Lenovo ThinkPad L13 Yoga (20R50007UK)");
        assert_eq!(info.cpu, "Intel Core i5-10210U @ 1.60 GHz (2 cores / 4 threads)");
    }

    #[test]
    fn the_report_has_the_requested_rows_in_order_and_aligned_values() {
        for tree in [laptop(), Tree::new()] {
            let report = collect_from(&tree.0, "0.3.0 (abc1234)".into()).render();
            let lines: Vec<&str> = report.lines().collect();
            let labels: Vec<&str> = lines.iter().map(|l| l.split_whitespace().next().unwrap()).collect();
            assert!(["Model", "Motherboard"].contains(&labels[0]), "{labels:?}");
            assert_eq!(labels[1..], ["CPU", "Memory", "Disk", "Uptime", "OS", "Monogram"]);
            // Every value starts in the same column, two spaces after the longest label.
            let column = lines[0].len() - lines[0].trim_start_matches(|c: char| !c.is_whitespace()).trim_start().len();
            for line in &lines {
                let value = line.split_once("  ").map(|(_, rest)| rest.trim_start()).unwrap();
                assert_eq!(line.len() - value.len(), column, "misaligned: {line:?}");
            }
            assert!(lines[6].ends_with("0.3.0 (abc1234)"));
        }
    }

    #[test]
    fn memory_and_disk_rows_use_the_formatted_parsed_values() {
        let info = collect_from(&laptop().0, "x".into());
        let report = info.render();
        let memory = report.lines().find(|l| l.starts_with("Memory")).unwrap();
        // 2,895,148 kB of 7,702,148 kB is 37.6%, which rounds to 38%.
        assert!(memory.ends_with(&format!("{} / {} (38%)", size(info.memory_used), size(info.memory_total))), "{memory}");
        assert!(report.lines().any(|l| l.starts_with("Disk") && l.ends_with(&format!("{} total", size(info.disk_total)))));
    }

    #[test]
    fn the_report_never_contains_a_user_or_host_name() {
        let tree = laptop();
        tree.file("etc/hostname", "secret-host\n").file("proc/sys/kernel/hostname", "secret-host\n");
        let report = collect_from(&tree.0, client_from("0.3.0", None, None)).render();
        assert!(!report.contains("secret-host"));
        for variable in ["USER", "LOGNAME"] {
            if let Ok(value) = std::env::var(variable) {
                assert!(value.len() < 3 || !report.contains(&value), "{variable} must not appear");
            }
        }
        let live = collect().render();
        if let Ok(host) = fs::read_to_string("/proc/sys/kernel/hostname") {
            let host = host.trim();
            assert!(host.len() < 3 || !live.contains(host), "the host name must not appear");
        }
    }

    #[test]
    fn disks_count_only_physical_fixed_drives() {
        let tree = laptop();
        // 515521536 sectors * 512; the removable stick, loop and zram devices are ignored.
        assert_eq!(disks(&tree.0), 515_521_536 * 512);
        assert_eq!(disks(&Tree::new().0), 0, "no drives at all is zero, not an error");
    }

    #[test]
    fn a_desktop_shows_its_motherboard_and_skips_firmware_placeholders() {
        let tree = Tree::new();
        tree.file("sys/class/dmi/id/chassis_type", "3\n")
            .file("sys/class/dmi/id/board_vendor", "ASUSTeK COMPUTER INC.\n")
            .file("sys/class/dmi/id/board_name", "PRIME B450M-A\n")
            .file("sys/class/dmi/id/product_name", "System Product Name\n");
        assert_eq!(machine(&tree.0), ("Motherboard", "ASUSTeK COMPUTER INC. PRIME B450M-A".into()));
        let placeholder = Tree::new();
        placeholder
            .file("sys/class/dmi/id/chassis_type", "3\n")
            .file("sys/class/dmi/id/board_vendor", "To be filled by O.E.M.\n")
            .file("sys/class/dmi/id/board_name", "Default string\n");
        assert_eq!(machine(&placeholder.0), ("Motherboard", "Unknown".into()));
    }

    #[test]
    fn a_laptop_without_a_separate_marketing_name_shows_its_product_name_once() {
        let tree = Tree::new();
        tree.file("sys/class/dmi/id/chassis_type", "10\n")
            .file("sys/class/dmi/id/sys_vendor", "Dell Inc.\n")
            .file("sys/class/dmi/id/product_name", "XPS 13 9310\n")
            .file("sys/class/dmi/id/product_version", "XPS 13 9310\n");
        assert_eq!(machine(&tree.0), ("Model", "Dell Inc. XPS 13 9310".into()));
    }

    #[test]
    fn missing_files_degrade_to_sensible_defaults_instead_of_failing() {
        let empty = Tree::new();
        let info = collect_from(&empty.0, "0.3.0".into());
        assert_eq!(info.os, "Linux");
        assert_eq!(info.cpu, "Unknown processor");
        assert_eq!(info.machine.1, "Unknown");
        let report = info.render();
        let memory = report.lines().find(|l| l.starts_with("Memory")).unwrap();
        assert_eq!(memory.split_whitespace().skip(1).collect::<Vec<_>>().join(" "), "0 MiB / 0 MiB (0%)");
        assert!(report.contains("less than a minute"));
    }

    #[test]
    fn cpu_names_are_tidied_and_arm_without_topology_reports_threads() {
        let arm = Tree::new();
        arm.file("proc/cpuinfo", "processor\t: 0\nprocessor\t: 1\nHardware\t: BCM2711\n");
        assert_eq!(cpu(&arm.0), "BCM2711 (2 threads)");
        let amd = Tree::new();
        amd.file("proc/cpuinfo", "processor : 0\nmodel name : AMD Ryzen(TM) 7 5800X 8-Core Processor\ncore id : 0\n");
        assert_eq!(cpu(&amd.0), "AMD Ryzen 7 5800X 8-Core Processor (1 threads)");
    }

    #[test]
    fn units_and_uptime_read_naturally() {
        assert_eq!(size(7_886_360_576), "7.34 GiB");
        assert_eq!(size(2 * 1024 * 1024 * 1024 * 1024), "2.00 TiB");
        assert_eq!(size(512 * 1024 * 1024), "512 MiB");
        assert_eq!(uptime(59), "less than a minute");
        assert_eq!(uptime(61), "1 minute");
        assert_eq!(uptime(3600 + 120), "1 hour, 2 minutes");
        assert_eq!(uptime(86_400 * 3), "3 days");
        assert_eq!(uptime(86_400 + 3600 + 60), "1 day, 1 hour, 1 minute");
    }

    #[test]
    fn the_client_line_shows_the_commit_and_whether_the_tree_was_dirty() {
        assert_eq!(client_from("0.3.0", Some("4d98353"), Some("0")), "0.3.0 (4d98353)");
        assert_eq!(client_from("0.3.0", Some("4d98353"), Some("1")), "0.3.0 (4d98353, uncommitted changes)");
        assert_eq!(client_from("0.3.0", None, None), "0.3.0", "a build without git history still reports its version");
        assert_eq!(client_from("0.3.0", Some(""), Some("1")), "0.3.0");
    }

    #[test]
    fn only_the_exact_command_is_neofetch() {
        for yes in ["/neofetch", "  /neofetch\n", "/NeoFetch"] {
            assert!(is_neofetch(yes), "{yes:?}");
        }
        for no in ["", "neofetch", "/neofetch now", "/neofetch\nhello", "hello /neofetch", "//neofetch", "/neofetchx"] {
            assert!(!is_neofetch(no), "{no:?}");
        }
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "prints this machine's report: cargo test live -- --ignored --nocapture"]
    fn print_this_machines_report() {
        eprintln!("\n{}\n", super::collect().render());
    }
}
