//! `HardwareTopology` introspection (§17.4, Rev 4): the machine shape that
//! fmn-runtime's `ExecutionPlan` derivation consumes.
//!
//! Behavior keys on **introspected topology, never marketing names** (§17.6).
//! What the planner needs to know, this struct carries: logical/physical
//! cores, packages, SMT siblings, performance classes (P/E), L2/L3 cache
//! domains (the CCD sharding §17.6's playbooks are built on), NUMA nodes,
//! Windows processor groups, the SIMD build-tier the CPU supports, and
//! available memory.
//!
//! Introspection obeys the capability doctrine: Linux detection reads sysfs
//! **through** [`FileSystem`], so a synthetic machine is a fixture handed to
//! [`HardwareTopology::detect_linux`] — that is how the aarch64 big.LITTLE
//! and Windows processor-group shapes are tested on any host.
//!
//! **Windows processor groups (§17.4):** systems above 64 logical processors
//! span groups, and explicit scheduling code must know it. The model here is
//! groups of at most 64 logical CPUs; [`HardwareTopology::from_group_sizes`]
//! builds synthetic groupings and [`windows_group_split`] is the documented
//! split rule. Native Windows introspection is not implemented (it needs
//! Win32 calls outside the governed closure's current surface); Windows
//! builds use [`HardwareTopology::fallback`], which already applies the
//! group split — so no scheduling consumer can be written that ignores
//! groups. R18's functional CI keeps this honest.

use crate::fs::{FileSystem, FsError};
use std::fmt;
use std::path::{Path, PathBuf};

/// The SIMD build tiers (§17.3, capped by R22 at exactly these four).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SimdTier {
    /// Portable scalar/std::simd baseline — always available.
    Portable,
    /// x86-64-v3: AVX2 + FMA + BMI2 class.
    X86_64V3,
    /// x86-64-v4: AVX-512 (F/BW/VL/DQ) class.
    X86_64V4,
    /// aarch64 with NEON (the aarch64 baseline).
    Aarch64Neon,
}

impl SimdTier {
    /// The tier's canonical name (matches SUITE.lock / release artifact
    /// naming).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::X86_64V3 => "x86-64-v3",
            Self::X86_64V4 => "x86-64-v4",
            Self::Aarch64Neon => "aarch64-neon",
        }
    }
}

/// Performance class of a logical CPU (P/E cores).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PerfClass {
    /// Performance core (or any core on a uniform machine).
    Performance,
    /// Efficiency core.
    Efficiency,
}

/// One logical CPU.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LogicalCpu {
    /// Kernel CPU id.
    pub id: u32,
    /// Physical package (socket) id.
    pub package_id: u32,
    /// Core id within the package; logical CPUs sharing `(package_id,
    /// core_id)` are SMT siblings.
    pub core_id: u32,
    /// Scheduler capacity (arm `cpu_capacity`), if exposed.
    pub capacity: Option<u32>,
    /// Maximum frequency in kHz, if exposed.
    pub max_freq_khz: Option<u64>,
    /// Derived performance class.
    pub class: PerfClass,
}

/// A cache domain: one cache of `level` shared by `cpus`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CacheDomain {
    /// Cache level (2 or 3 are recorded).
    pub level: u8,
    /// Size in bytes, if exposed.
    pub size_bytes: Option<u64>,
    /// The logical CPUs sharing this cache, ascending.
    pub cpus: Vec<u32>,
}

/// A NUMA node and its CPUs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NumaNode {
    /// Node id.
    pub id: u32,
    /// The node's logical CPUs, ascending.
    pub cpus: Vec<u32>,
}

/// A processor group (Windows scheduling domain; one group elsewhere).
/// Invariant: at most 64 logical CPUs per group.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProcessorGroup {
    /// Group id.
    pub id: u32,
    /// The group's logical CPUs, ascending.
    pub cpus: Vec<u32>,
}

/// The introspected machine shape (§17.4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HardwareTopology {
    /// Every online logical CPU, ascending by id.
    pub cpus: Vec<LogicalCpu>,
    /// Distinct physical cores (unique `(package, core)` pairs).
    pub physical_cores: u32,
    /// Distinct physical packages (sockets).
    pub packages: u32,
    /// L2 cache domains (E-core clusters share L2; SMT siblings share L2 on
    /// x86).
    pub l2_domains: Vec<CacheDomain>,
    /// L3 cache domains — the CCD boundaries §17.6's sharding keys on.
    pub l3_domains: Vec<CacheDomain>,
    /// NUMA nodes (one node on a desktop part).
    pub numa_nodes: Vec<NumaNode>,
    /// Processor groups (≤ 64 CPUs each; exactly one group on machines that
    /// fit, per the Windows model above).
    pub processor_groups: Vec<ProcessorGroup>,
    /// The SIMD tier this machine supports.
    pub simd_tier: SimdTier,
    /// Total physical memory in bytes, if known.
    pub total_memory_bytes: Option<u64>,
}

/// A topology introspection failure.
#[derive(Debug)]
pub enum TopologyError {
    /// A required sysfs/procfs file is missing.
    Missing {
        /// The missing path.
        path: PathBuf,
    },
    /// A file existed but did not parse.
    Parse {
        /// The offending path.
        path: PathBuf,
        /// What was wrong.
        detail: String,
    },
    /// A structural invariant failed (e.g. a processor group above 64).
    Invalid {
        /// What was wrong.
        detail: String,
    },
}

impl fmt::Display for TopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { path } => write!(f, "topology file missing: {}", path.display()),
            Self::Parse { path, detail } => {
                write!(f, "topology parse failure at {}: {detail}", path.display())
            }
            Self::Invalid { detail } => write!(f, "invalid topology: {detail}"),
        }
    }
}

impl std::error::Error for TopologyError {}

/// Parse a kernel CPU list (`"0-7,64-71"`) into ascending ids.
///
/// # Errors
/// [`TopologyError::Invalid`] on malformed input.
pub fn parse_cpu_list(s: &str) -> Result<Vec<u32>, TopologyError> {
    let s = s.trim();
    let mut out = Vec::new();
    if s.is_empty() {
        return Ok(out);
    }
    for part in s.split(',') {
        let part = part.trim();
        if let Some((a, b)) = part.split_once('-') {
            let a: u32 = a.parse().map_err(|_| invalid_list(s))?;
            let b: u32 = b.parse().map_err(|_| invalid_list(s))?;
            if a > b {
                return Err(invalid_list(s));
            }
            out.extend(a..=b);
        } else {
            out.push(part.parse().map_err(|_| invalid_list(s))?);
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn invalid_list(s: &str) -> TopologyError {
    TopologyError::Invalid {
        detail: format!("malformed CPU list {s:?}"),
    }
}

/// Render ascending CPU ids as a compact kernel-style list (`"0-7,64-71"`),
/// the inverse of [`parse_cpu_list`]. Used by the snapshot format.
#[must_use]
pub fn format_cpu_list(cpus: &[u32]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < cpus.len() {
        let start = cpus[i];
        let mut end = start;
        while i + 1 < cpus.len() && cpus[i + 1] == end + 1 {
            i += 1;
            end = cpus[i];
        }
        if start == end {
            parts.push(start.to_string());
        } else {
            parts.push(format!("{start}-{end}"));
        }
        i += 1;
    }
    parts.join(",")
}

/// The documented Windows split of `logical` processors into groups: full
/// groups of 64, then the remainder. (Real Windows assigns by NUMA
/// proximity; the invariant scheduling code must honor — no group above 64,
/// possibly several groups — is identical, and that is what consumers key
/// on.)
#[must_use]
pub fn windows_group_split(logical: u32) -> Vec<u32> {
    let mut sizes = Vec::new();
    let mut left = logical;
    while left > 64 {
        sizes.push(64);
        left -= 64;
    }
    if left > 0 || sizes.is_empty() {
        sizes.push(left);
    }
    sizes
}

/// Detect the SIMD tier of the running CPU. Safe feature detection via
/// `std::arch`; dispatch stays by build tier (§17.3) — this value reports
/// what the machine *could* run, for `fmn doctor` and artifact selection.
#[must_use]
pub fn detect_simd_tier() -> SimdTier {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512bw")
            && std::arch::is_x86_feature_detected!("avx512vl")
            && std::arch::is_x86_feature_detected!("avx512dq")
        {
            SimdTier::X86_64V4
        } else if std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("fma")
            && std::arch::is_x86_feature_detected!("bmi2")
        {
            SimdTier::X86_64V3
        } else {
            SimdTier::Portable
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // NEON is architecturally mandatory on aarch64.
        SimdTier::Aarch64Neon
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        SimdTier::Portable
    }
}

const SYS_CPU: &str = "/sys/devices/system/cpu";
const SYS_NODE: &str = "/sys/devices/system/node";
const PROC_MEMINFO: &str = "/proc/meminfo";

impl HardwareTopology {
    /// Logical CPU count.
    #[must_use]
    pub fn logical_cores(&self) -> u32 {
        u32::try_from(self.cpus.len()).unwrap_or(u32::MAX)
    }

    /// Whether SMT is active (any core carries more than one logical CPU).
    #[must_use]
    pub fn smt_active(&self) -> bool {
        self.logical_cores() > self.physical_cores
    }

    /// The topology of the running machine. Linux introspects sysfs; other
    /// hosts (and a sysfs read failure) fall back to
    /// [`HardwareTopology::fallback`] over `available_parallelism` — the
    /// planner always gets *a* topology, just a flat one.
    #[must_use]
    pub fn current() -> Self {
        let logical = std::thread::available_parallelism()
            .map(|n| u32::try_from(n.get()).unwrap_or(1))
            .unwrap_or(1);
        if cfg!(target_os = "linux") {
            Self::detect_linux(&crate::fs::StdFs).unwrap_or_else(|_| Self::fallback(logical))
        } else {
            Self::fallback(logical)
        }
    }

    /// A flat topology from a logical-CPU count: one package, no SMT
    /// knowledge, no cache/NUMA structure, groups per the Windows split,
    /// the detected SIMD tier, and unknown memory.
    #[must_use]
    pub fn fallback(logical: u32) -> Self {
        let logical = logical.max(1);
        let cpus: Vec<LogicalCpu> = (0..logical)
            .map(|id| LogicalCpu {
                id,
                package_id: 0,
                core_id: id,
                capacity: None,
                max_freq_khz: None,
                class: PerfClass::Performance,
            })
            .collect();
        let mut topo = Self {
            physical_cores: logical,
            packages: 1,
            l2_domains: Vec::new(),
            l3_domains: Vec::new(),
            numa_nodes: vec![NumaNode {
                id: 0,
                cpus: cpus.iter().map(|c| c.id).collect(),
            }],
            processor_groups: Vec::new(),
            simd_tier: detect_simd_tier(),
            total_memory_bytes: None,
            cpus,
        };
        topo.processor_groups = group_by_split(&topo.cpu_ids());
        topo
    }

    /// A synthetic topology from explicit processor-group sizes — the
    /// Windows-model constructor the synthetic tests drive.
    ///
    /// # Errors
    /// [`TopologyError::Invalid`] if any group exceeds 64 CPUs or the total
    /// is zero.
    pub fn from_group_sizes(sizes: &[u32]) -> Result<Self, TopologyError> {
        let total: u32 = sizes.iter().sum();
        if total == 0 {
            return Err(TopologyError::Invalid {
                detail: "zero logical CPUs".to_string(),
            });
        }
        if let Some(&too_big) = sizes.iter().find(|&&s| s > 64) {
            return Err(TopologyError::Invalid {
                detail: format!("processor group of {too_big} CPUs exceeds the 64-CPU limit"),
            });
        }
        let mut topo = Self::fallback(total);
        let mut groups = Vec::new();
        let mut next = 0u32;
        for (gid, &size) in sizes.iter().enumerate() {
            groups.push(ProcessorGroup {
                id: u32::try_from(gid).unwrap_or(u32::MAX),
                cpus: (next..next + size).collect(),
            });
            next += size;
        }
        topo.processor_groups = groups;
        Ok(topo)
    }

    fn cpu_ids(&self) -> Vec<u32> {
        self.cpus.iter().map(|c| c.id).collect()
    }

    /// Introspect a Linux sysfs/procfs tree through the filesystem
    /// capability. Required: the online-CPU list. Everything else degrades
    /// gracefully (missing optional files leave `None`s / empty domains).
    ///
    /// # Errors
    /// [`TopologyError`] when the online list is missing or malformed.
    pub fn detect_linux(fs: &dyn FileSystem) -> Result<Self, TopologyError> {
        let online_path = PathBuf::from(SYS_CPU).join("online");
        let online = read_string(fs, &online_path)?;
        let ids = parse_cpu_list(&online)?;
        if ids.is_empty() {
            return Err(TopologyError::Invalid {
                detail: "empty online CPU list".to_string(),
            });
        }

        let mut cpus = Vec::with_capacity(ids.len());
        for &id in &ids {
            let base = PathBuf::from(SYS_CPU).join(format!("cpu{id}"));
            let package_id = read_optional_u32(fs, &base.join("topology/physical_package_id"))
                .unwrap_or_default();
            let core_id = read_optional_u32(fs, &base.join("topology/core_id")).unwrap_or(id);
            let capacity = read_optional_u32(fs, &base.join("cpu_capacity"));
            let max_freq_khz = read_optional_u64(fs, &base.join("cpufreq/cpuinfo_max_freq"));
            cpus.push(LogicalCpu {
                id,
                package_id,
                core_id,
                capacity,
                max_freq_khz,
                class: PerfClass::Performance, // assigned below
            });
        }
        assign_perf_classes(&mut cpus);

        let mut cores: Vec<(u32, u32)> = cpus.iter().map(|c| (c.package_id, c.core_id)).collect();
        cores.sort_unstable();
        cores.dedup();
        let mut packages: Vec<u32> = cpus.iter().map(|c| c.package_id).collect();
        packages.sort_unstable();
        packages.dedup();

        let (l2_domains, l3_domains) = read_cache_domains(fs, &ids);
        let numa_nodes = read_numa_nodes(fs).unwrap_or_else(|| {
            vec![NumaNode {
                id: 0,
                cpus: ids.clone(),
            }]
        });
        let total_memory_bytes = read_meminfo_total(fs);

        Ok(Self {
            physical_cores: u32::try_from(cores.len()).unwrap_or(u32::MAX),
            packages: u32::try_from(packages.len()).unwrap_or(u32::MAX),
            l2_domains,
            l3_domains,
            numa_nodes,
            processor_groups: group_by_split(&ids),
            simd_tier: detect_simd_tier(),
            total_memory_bytes,
            cpus,
        })
    }

    /// A deterministic text rendering — the committed fixture format
    /// (`fixtures/*.snapshot.txt`) and `fmn doctor`'s raw form.
    #[must_use]
    pub fn snapshot_text(&self) -> String {
        let mut out = String::from("# fmn hardware-topology snapshot v1\n");
        let push = |out: &mut String, line: String| {
            out.push_str(&line);
            out.push('\n');
        };
        push(&mut out, format!("logical_cores\t{}", self.logical_cores()));
        push(&mut out, format!("physical_cores\t{}", self.physical_cores));
        push(&mut out, format!("packages\t{}", self.packages));
        push(&mut out, format!("smt\t{}", self.smt_active()));
        push(&mut out, format!("simd_tier\t{}", self.simd_tier.name()));
        push(
            &mut out,
            format!(
                "memory_bytes\t{}",
                self.total_memory_bytes
                    .map_or_else(|| "-".to_string(), |m| m.to_string())
            ),
        );
        for c in &self.cpus {
            push(
                &mut out,
                format!(
                    "cpu\t{}\tpackage\t{}\tcore\t{}\tclass\t{}\tcapacity\t{}\tmax_freq_khz\t{}",
                    c.id,
                    c.package_id,
                    c.core_id,
                    match c.class {
                        PerfClass::Performance => "P",
                        PerfClass::Efficiency => "E",
                    },
                    c.capacity
                        .map_or_else(|| "-".to_string(), |v| v.to_string()),
                    c.max_freq_khz
                        .map_or_else(|| "-".to_string(), |v| v.to_string()),
                ),
            );
        }
        for (label, domains) in [("l2", &self.l2_domains), ("l3", &self.l3_domains)] {
            for d in domains {
                push(
                    &mut out,
                    format!(
                        "{label}\tsize\t{}\tcpus\t{}",
                        d.size_bytes
                            .map_or_else(|| "-".to_string(), |v| v.to_string()),
                        format_cpu_list(&d.cpus)
                    ),
                );
            }
        }
        for n in &self.numa_nodes {
            push(
                &mut out,
                format!("numa\t{}\tcpus\t{}", n.id, format_cpu_list(&n.cpus)),
            );
        }
        for g in &self.processor_groups {
            push(
                &mut out,
                format!("group\t{}\tcpus\t{}", g.id, format_cpu_list(&g.cpus)),
            );
        }
        out
    }
}

/// Assign P/E classes: if the machine exposes heterogeneous capacities, the
/// maximum-capacity CPUs are Performance and the rest Efficiency; failing
/// that, heterogeneous max frequencies decide the same way (with a 20%
/// hysteresis so binning variance is not misread as E-cores); a uniform
/// machine is all Performance.
fn assign_perf_classes(cpus: &mut [LogicalCpu]) {
    let capacities: Vec<u32> = cpus.iter().filter_map(|c| c.capacity).collect();
    if let Some(&max) = capacities.iter().max()
        && capacities.iter().any(|&c| c != max)
        && capacities.len() == cpus.len()
    {
        for c in cpus {
            c.class = if c.capacity == Some(max) {
                PerfClass::Performance
            } else {
                PerfClass::Efficiency
            };
        }
        return;
    }
    let freqs: Vec<u64> = cpus.iter().filter_map(|c| c.max_freq_khz).collect();
    if let Some(&max) = freqs.iter().max()
        && freqs.len() == cpus.len()
    {
        let threshold = max - max / 5;
        if freqs.iter().any(|&f| f < threshold) {
            for c in cpus {
                c.class = if c.max_freq_khz.unwrap_or(0) >= threshold {
                    PerfClass::Performance
                } else {
                    PerfClass::Efficiency
                };
            }
        }
    }
}

fn group_by_split(ids: &[u32]) -> Vec<ProcessorGroup> {
    let mut groups = Vec::new();
    for (gid, chunk) in ids.chunks(64).enumerate() {
        groups.push(ProcessorGroup {
            id: u32::try_from(gid).unwrap_or(u32::MAX),
            cpus: chunk.to_vec(),
        });
    }
    groups
}

fn read_string(fs: &dyn FileSystem, path: &Path) -> Result<String, TopologyError> {
    match fs.read_to_string(path) {
        Ok(s) => Ok(s),
        Err(FsError::NotFound { .. }) => Err(TopologyError::Missing {
            path: path.to_path_buf(),
        }),
        Err(e) => Err(TopologyError::Parse {
            path: path.to_path_buf(),
            detail: e.to_string(),
        }),
    }
}

fn read_optional_u32(fs: &dyn FileSystem, path: &Path) -> Option<u32> {
    fs.read_to_string(path).ok()?.trim().parse().ok()
}

fn read_optional_u64(fs: &dyn FileSystem, path: &Path) -> Option<u64> {
    fs.read_to_string(path).ok()?.trim().parse().ok()
}

/// Collect deduplicated L2/L3 cache domains from every CPU's cache indexes.
fn read_cache_domains(fs: &dyn FileSystem, ids: &[u32]) -> (Vec<CacheDomain>, Vec<CacheDomain>) {
    let mut l2: Vec<CacheDomain> = Vec::new();
    let mut l3: Vec<CacheDomain> = Vec::new();
    for &id in ids {
        for index in 0..8u32 {
            let base = PathBuf::from(SYS_CPU).join(format!("cpu{id}/cache/index{index}"));
            let Some(level) = read_optional_u32(fs, &base.join("level")) else {
                continue;
            };
            if level != 2 && level != 3 {
                continue;
            }
            let cache_type = fs
                .read_to_string(&base.join("type"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if cache_type != "Unified" && cache_type != "Data" {
                continue;
            }
            let Ok(shared) = fs.read_to_string(&base.join("shared_cpu_list")) else {
                continue;
            };
            let Ok(cpus) = parse_cpu_list(&shared) else {
                continue;
            };
            let size_bytes = fs
                .read_to_string(&base.join("size"))
                .ok()
                .and_then(|s| parse_cache_size(s.trim()));
            let domain = CacheDomain {
                level: u8::try_from(level).unwrap_or(u8::MAX),
                size_bytes,
                cpus,
            };
            let bucket = if level == 2 { &mut l2 } else { &mut l3 };
            if !bucket.iter().any(|d| d.cpus == domain.cpus) {
                bucket.push(domain);
            }
        }
    }
    for bucket in [&mut l2, &mut l3] {
        bucket.sort_by_key(|d| d.cpus.first().copied().unwrap_or(0));
    }
    (l2, l3)
}

/// Parse a sysfs cache size (`"32768K"`, `"32M"`) into bytes.
fn parse_cache_size(s: &str) -> Option<u64> {
    if let Some(k) = s.strip_suffix('K') {
        k.parse::<u64>().ok().map(|v| v * 1024)
    } else if let Some(m) = s.strip_suffix('M') {
        m.parse::<u64>().ok().map(|v| v * 1024 * 1024)
    } else {
        s.parse::<u64>().ok()
    }
}

/// Read NUMA nodes from `/sys/devices/system/node/node*/cpulist`; `None`
/// when the node directory is absent (the caller substitutes one node).
fn read_numa_nodes(fs: &dyn FileSystem) -> Option<Vec<NumaNode>> {
    let mut nodes = Vec::new();
    for id in 0..1024u32 {
        let path = PathBuf::from(SYS_NODE).join(format!("node{id}/cpulist"));
        match fs.read_to_string(&path) {
            Ok(list) => {
                if let Ok(cpus) = parse_cpu_list(&list) {
                    nodes.push(NumaNode { id, cpus });
                }
            }
            Err(_) => break,
        }
    }
    if nodes.is_empty() { None } else { Some(nodes) }
}

/// `MemTotal` from `/proc/meminfo`, in bytes.
fn read_meminfo_total(fs: &dyn FileSystem) -> Option<u64> {
    let text = fs.read_to_string(Path::new(PROC_MEMINFO)).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_list_round_trips() {
        for (text, ids) in [
            ("0-3", vec![0, 1, 2, 3]),
            ("0,2,4", vec![0, 2, 4]),
            ("0-1,64-65", vec![0, 1, 64, 65]),
            ("7", vec![7]),
            ("", vec![]),
        ] {
            assert_eq!(parse_cpu_list(text).unwrap(), ids, "{text}");
            assert_eq!(parse_cpu_list(&format_cpu_list(&ids)).unwrap(), ids);
        }
        assert!(parse_cpu_list("3-1").is_err());
        assert!(parse_cpu_list("a-b").is_err());
    }

    #[test]
    fn windows_split_never_exceeds_64() {
        assert_eq!(windows_group_split(1), vec![1]);
        assert_eq!(windows_group_split(64), vec![64]);
        assert_eq!(windows_group_split(96), vec![64, 32]);
        assert_eq!(windows_group_split(128), vec![64, 64]);
        assert_eq!(windows_group_split(192), vec![64, 64, 64]);
        assert_eq!(windows_group_split(0), vec![0]);
    }

    #[test]
    fn cache_size_parses_k_and_m() {
        assert_eq!(parse_cache_size("32768K"), Some(32768 * 1024));
        assert_eq!(parse_cache_size("32M"), Some(32 * 1024 * 1024));
        assert_eq!(parse_cache_size("512"), Some(512));
        assert_eq!(parse_cache_size("x"), None);
    }
}
