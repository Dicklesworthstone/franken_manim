//! Resource ceilings visible to the calling Linux thread. All input goes
//! through the filesystem capability, including procfs discovery: cgroup
//! controllers need not be mounted at their usual `/sys/fs/cgroup` paths.

use super::{MAX_PROC_STATUS_BYTES, MAX_TOPOLOGY_SCALAR_BYTES, TopologyError, parse_cpu_list};
use crate::fs::{FileSystem, FsError};
use std::path::{Path, PathBuf};

const THREAD_STATUS: &str = "/proc/thread-self/status";
const PROCESS_STATUS: &str = "/proc/self/status";
const MEMBERSHIP: &str = "/proc/self/cgroup";
const THREAD_MEMBERSHIP: &str = "/proc/thread-self/cgroup";
const MOUNTS: &str = "/proc/self/mountinfo";
const MAX_MEMBERSHIP_BYTES: usize = 64 * 1024;
const MAX_MOUNTINFO_BYTES: usize = 1024 * 1024;
const MAX_MEMBERSHIPS: usize = 128;
const MAX_MOUNT_ENTRIES: usize = 16 * 1024;
const MAX_CONTROLLER_MOUNTS: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_PATH_COMPONENTS: usize = 128;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct LinuxLimits {
    pub allowed_cpus: Option<Vec<u32>>,
    pub cpu_quota: Option<u32>,
    pub memory_bytes: Option<u64>,
}

pub(super) fn read(fs: &dyn FileSystem) -> Result<LinuxLimits, TopologyError> {
    let mut limits = LinuxLimits {
        allowed_cpus: read_affinity(fs)?,
        ..LinuxLimits::default()
    };
    let process = read_membership(fs, Path::new(MEMBERSHIP))?;
    let thread = read_membership(fs, Path::new(THREAD_MEMBERSHIP))?;
    if process.is_none() && thread.is_none() {
        return Ok(limits);
    }
    // CPU controllers can move individual threads; newly created render
    // workers inherit the caller's membership. Memory charges belong to the
    // process's address space, so keep the leader's memory hierarchy.
    let unknown = Memberships::default();
    let cpu_membership = thread.as_ref().or(process.as_ref()).unwrap_or(&unknown);
    let memory_membership = process.as_ref().or(thread.as_ref()).unwrap_or(&unknown);
    let Some(mountinfo) = optional(fs, Path::new(MOUNTS), MAX_MOUNTINFO_BYTES)? else {
        return Ok(limits);
    };
    let mounts = parse_mounts(&mountinfo)?;

    if let Some(mount) = resolve(
        &mounts,
        cpu_membership.unified.as_deref(),
        Controller::Unified,
    )? {
        for_ancestors(&mount, |directory| {
            let cpu_path = directory.join("cpu.max");
            if let Some(value) = optional(fs, &cpu_path, MAX_TOPOLOGY_SCALAR_BYTES)? {
                minimize(&mut limits.cpu_quota, parse_cpu_max(&value, &cpu_path)?);
            }
            Ok(())
        })?;
    }
    if let Some(mount) = resolve(
        &mounts,
        memory_membership.unified.as_deref(),
        Controller::Unified,
    )? {
        for_ancestors(&mount, |directory| {
            let memory_path = directory.join("memory.max");
            if let Some(value) = optional(fs, &memory_path, MAX_TOPOLOGY_SCALAR_BYTES)? {
                minimize(
                    &mut limits.memory_bytes,
                    parse_memory_max(&value, &memory_path)?,
                );
            }
            Ok(())
        })?;
    }
    if let Some(mount) = resolve(&mounts, cpu_membership.cpu.as_deref(), Controller::Cpu)? {
        for_ancestors(&mount, |directory| {
            let quota_path = directory.join("cpu.cfs_quota_us");
            let period_path = directory.join("cpu.cfs_period_us");
            let quota = optional(fs, &quota_path, MAX_TOPOLOGY_SCALAR_BYTES)?;
            let period = optional(fs, &period_path, MAX_TOPOLOGY_SCALAR_BYTES)?;
            match (quota, period) {
                (None, None) => {}
                (Some(quota), Some(period)) => {
                    let period = positive(&period, &period_path, "CPU period")?;
                    if quota.trim() != "-1" {
                        let quota = positive(&quota, &quota_path, "CPU quota")?;
                        minimize(&mut limits.cpu_quota, Some(quota_workers(quota, period)));
                    }
                }
                (Some(_), None) => {
                    return Err(parse_error(
                        &period_path,
                        "CPU quota exists without its period",
                    ));
                }
                (None, Some(_)) => {
                    return Err(parse_error(
                        &quota_path,
                        "CPU period exists without its quota",
                    ));
                }
            }
            Ok(())
        })?;
    }
    if let Some(mount) = resolve(
        &mounts,
        memory_membership.memory.as_deref(),
        Controller::Memory,
    )? {
        let mut ancestors_apply = true;
        for_ancestors(&mount, |directory| {
            if !ancestors_apply {
                return Ok(());
            }
            if directory != mount.group.as_path() {
                let hierarchy_path = directory.join("memory.use_hierarchy");
                if let Some(value) = optional(fs, &hierarchy_path, MAX_TOPOLOGY_SCALAR_BYTES)? {
                    match value.trim() {
                        "0" => {
                            // Older v1 kernels permit a nonhierarchical
                            // subtree. Its ancestor's limit covers that
                            // ancestor's own tasks, not this process.
                            ancestors_apply = false;
                            return Ok(());
                        }
                        "1" => {}
                        _ => {
                            return Err(parse_error(
                                &hierarchy_path,
                                "memory.use_hierarchy must be 0 or 1",
                            ));
                        }
                    }
                }
            }
            let path = directory.join("memory.limit_in_bytes");
            if let Some(value) = optional(fs, &path, MAX_TOPOLOGY_SCALAR_BYTES)? {
                // v1 represents unlimited memory with a very large numeric
                // value, varying with architecture/page size. The caller
                // intersects this with physical RAM, avoiding magic sentinels.
                minimize(
                    &mut limits.memory_bytes,
                    Some(unsigned(&value, &path, "memory limit")?),
                );
            }
            Ok(())
        })?;
    }
    Ok(limits)
}

fn read_affinity(fs: &dyn FileSystem) -> Result<Option<Vec<u32>>, TopologyError> {
    for name in [THREAD_STATUS, PROCESS_STATUS] {
        let path = Path::new(name);
        let Some(status) = optional(fs, path, MAX_PROC_STATUS_BYTES)? else {
            continue;
        };
        let mut allowed = None;
        for line in status.lines() {
            let Some(value) = line.strip_prefix("Cpus_allowed_list:") else {
                continue;
            };
            if allowed.is_some() {
                return Err(parse_error(path, "duplicate Cpus_allowed_list"));
            }
            let cpus = parse_cpu_list(value)
                .map_err(|error| parse_error(path, format!("Cpus_allowed_list: {error}")))?;
            if cpus.is_empty() {
                return Err(parse_error(path, "empty Cpus_allowed_list"));
            }
            allowed = Some(cpus);
        }
        if allowed.is_some() {
            return Ok(allowed);
        }
    }
    Ok(None)
}

#[derive(Default)]
struct Memberships {
    unified: Option<PathBuf>,
    cpu: Option<PathBuf>,
    memory: Option<PathBuf>,
}

fn read_membership(
    fs: &dyn FileSystem,
    source: &Path,
) -> Result<Option<Memberships>, TopologyError> {
    optional(fs, source, MAX_MEMBERSHIP_BYTES)?
        .map(|text| parse_membership(&text, source))
        .transpose()
}

fn parse_membership(text: &str, source: &Path) -> Result<Memberships, TopologyError> {
    let mut result = Memberships::default();
    let mut seen = [false; 3];
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_MEMBERSHIPS {
            return Err(parse_error(source, "too many cgroup memberships"));
        }
        let mut fields = line.splitn(3, ':');
        let hierarchy = fields.next().unwrap_or_default();
        let controllers = fields
            .next()
            .ok_or_else(|| parse_error(source, "missing controller list"))?;
        let group = fields
            .next()
            .ok_or_else(|| parse_error(source, "missing cgroup path"))?;
        let hierarchy = unsigned(hierarchy, source, "hierarchy id")?;
        let group = absolute_path(group, source, true)?;
        if hierarchy == 0 && controllers.is_empty() {
            set_membership(&mut result.unified, &mut seen[0], group, source)?;
        } else if hierarchy == 0 || controllers.is_empty() {
            return Err(parse_error(
                source,
                "inconsistent hierarchy id and controller list",
            ));
        } else {
            for controller in controllers.split(',') {
                match controller {
                    "cpu" => set_membership(&mut result.cpu, &mut seen[1], group.clone(), source)?,
                    "memory" => {
                        set_membership(&mut result.memory, &mut seen[2], group.clone(), source)?
                    }
                    "" => return Err(parse_error(source, "empty controller name")),
                    _ => {}
                }
            }
        }
    }
    Ok(result)
}

fn set_membership(
    target: &mut Option<PathBuf>,
    seen: &mut bool,
    group: Option<PathBuf>,
    source: &Path,
) -> Result<(), TopologyError> {
    if *seen {
        return Err(parse_error(source, "duplicate controller membership"));
    }
    *seen = true;
    *target = group;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Controller {
    Unified,
    Cpu,
    Memory,
}

struct Mount {
    root: PathBuf,
    mountpoint: PathBuf,
    unified: bool,
    cpu: bool,
    memory: bool,
}

impl Mount {
    fn supports(&self, controller: Controller) -> bool {
        match controller {
            Controller::Unified => self.unified,
            Controller::Cpu => !self.unified && self.cpu,
            Controller::Memory => !self.unified && self.memory,
        }
    }
}

fn parse_mounts(text: &str) -> Result<Vec<Mount>, TopologyError> {
    let source = Path::new(MOUNTS);
    let mut result = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_MOUNT_ENTRIES {
            return Err(parse_error(source, "too many mount entries"));
        }
        let (before, after) = line
            .split_once(" - ")
            .ok_or_else(|| parse_error(source, "mount entry is missing its separator"))?;
        let mut fields = after.split_ascii_whitespace();
        let kind = fields
            .next()
            .ok_or_else(|| parse_error(source, "missing filesystem type"))?;
        if kind != "cgroup" && kind != "cgroup2" {
            continue;
        }
        let _device = fields
            .next()
            .ok_or_else(|| parse_error(source, "missing mount source"))?;
        let options = fields
            .next()
            .ok_or_else(|| parse_error(source, "missing superblock options"))?;
        if fields.next().is_some() {
            return Err(parse_error(source, "unexpected mount fields"));
        }
        let mut fields = before.split_ascii_whitespace();
        let _id = fields.next();
        let _parent = fields.next();
        let _device = fields.next();
        let root = fields
            .next()
            .ok_or_else(|| parse_error(source, "missing mount root"))?;
        let mountpoint = fields
            .next()
            .ok_or_else(|| parse_error(source, "missing mountpoint"))?;
        let _options = fields
            .next()
            .ok_or_else(|| parse_error(source, "missing mount options"))?;
        let root = absolute_path(&unescape_mount_path(root, source)?, source, true)?;
        let mountpoint =
            absolute_path(&unescape_mount_path(mountpoint, source)?, source, false)?
                .ok_or_else(|| parse_error(source, "mountpoint is outside the process root"))?;
        let Some(root) = root else {
            // An old mount retained across creation of a cgroup namespace can
            // report `/..` here. Its filesystem root is outside the visible
            // hierarchy; resolving it would require guessing hidden ancestry.
            continue;
        };
        if result.len() >= MAX_CONTROLLER_MOUNTS {
            return Err(parse_error(source, "too many cgroup controller mounts"));
        }
        result.push(Mount {
            root,
            mountpoint,
            unified: kind == "cgroup2",
            cpu: options.split(',').any(|option| option == "cpu"),
            memory: options.split(',').any(|option| option == "memory"),
        });
    }
    Ok(result)
}

/// Decode only mountinfo's documented octal escapes. Decoding bytes first
/// preserves UTF-8 paths, and a literal escaped backslash is not decoded twice.
fn unescape_mount_path(text: &str, source: &Path) -> Result<String, TopologyError> {
    let mut result = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            result.push(byte);
            continue;
        }
        let escape = [bytes.next(), bytes.next(), bytes.next()];
        result.push(match escape {
            [Some(b'0'), Some(b'4'), Some(b'0')] => b' ',
            [Some(b'0'), Some(b'1'), Some(b'1')] => b'\t',
            [Some(b'0'), Some(b'1'), Some(b'2')] => b'\n',
            [Some(b'1'), Some(b'3'), Some(b'4')] => b'\\',
            _ => return Err(parse_error(source, "invalid mount path escape")),
        });
    }
    String::from_utf8(result).map_err(|_| parse_error(source, "mount path is not UTF-8"))
}

fn absolute_path(
    text: &str,
    source: &Path,
    allow_namespace_parent: bool,
) -> Result<Option<PathBuf>, TopologyError> {
    if !text.starts_with('/') || text.len() > MAX_PATH_BYTES || text.contains('\0') {
        return Err(parse_error(
            source,
            "cgroup path must be a bounded absolute path",
        ));
    }
    let mut path = PathBuf::from("/");
    let mut count = 0;
    let mut outside_namespace = false;
    let mut seen_normal = false;
    for component in text.split('/').filter(|component| !component.is_empty()) {
        count += 1;
        if count > MAX_PATH_COMPONENTS {
            return Err(parse_error(source, "cgroup path exceeds the depth limit"));
        }
        match component {
            "." => return Err(parse_error(source, "cgroup path contains a dot component")),
            ".." if allow_namespace_parent && !seen_normal => outside_namespace = true,
            ".." => return Err(parse_error(source, "cgroup path contains parent traversal")),
            _ => {
                seen_normal = true;
                path.push(component);
            }
        }
    }
    if outside_namespace {
        Ok(None)
    } else {
        Ok(Some(path))
    }
}

struct ResolvedMount {
    mountpoint: PathBuf,
    group: PathBuf,
}

fn resolve(
    mounts: &[Mount],
    group: Option<&Path>,
    controller: Controller,
) -> Result<Option<ResolvedMount>, TopologyError> {
    let Some(group) = group else {
        return Ok(None);
    };
    // Prefer the mount exposing the greatest ancestor scope. More restricted
    // bind mounts of the same hierarchy cannot reveal additional hard limits.
    let mount = mounts
        .iter()
        .filter(|mount| mount.supports(controller) && group.starts_with(&mount.root))
        .min_by(|a, b| {
            a.root
                .components()
                .count()
                .cmp(&b.root.components().count())
                .then_with(|| a.mountpoint.cmp(&b.mountpoint))
        });
    let Some(mount) = mount else {
        return Ok(None);
    };
    let relative = group
        .strip_prefix(&mount.root)
        .map_err(|_| parse_error(Path::new(MOUNTS), "cgroup is outside its mount root"))?;
    let group = mount.mountpoint.join(relative);
    if group.as_os_str().len() > MAX_PATH_BYTES {
        return Err(parse_error(
            Path::new(MOUNTS),
            "resolved cgroup path exceeds the byte limit",
        ));
    }
    Ok(Some(ResolvedMount {
        mountpoint: mount.mountpoint.clone(),
        group,
    }))
}

fn for_ancestors(
    mount: &ResolvedMount,
    mut visit: impl FnMut(&Path) -> Result<(), TopologyError>,
) -> Result<(), TopologyError> {
    let mut directory = mount.group.clone();
    for _ in 0..=MAX_PATH_COMPONENTS {
        visit(&directory)?;
        if directory == mount.mountpoint {
            return Ok(());
        }
        if !directory.pop() || !directory.starts_with(&mount.mountpoint) {
            return Err(parse_error(
                Path::new(MOUNTS),
                "cgroup ancestor escaped its mountpoint",
            ));
        }
    }
    Err(parse_error(
        Path::new(MEMBERSHIP),
        "cgroup ancestor depth exceeds the limit",
    ))
}

fn parse_cpu_max(text: &str, path: &Path) -> Result<Option<u32>, TopologyError> {
    let mut fields = text.split_ascii_whitespace();
    let quota = fields
        .next()
        .ok_or_else(|| parse_error(path, "missing CPU quota"))?;
    let period = fields
        .next()
        .ok_or_else(|| parse_error(path, "missing CPU period"))?;
    if fields.next().is_some() {
        return Err(parse_error(path, "unexpected CPU quota fields"));
    }
    let period = positive(period, path, "CPU period")?;
    if quota == "max" {
        Ok(None)
    } else {
        Ok(Some(quota_workers(
            positive(quota, path, "CPU quota")?,
            period,
        )))
    }
}

fn parse_memory_max(text: &str, path: &Path) -> Result<Option<u64>, TopologyError> {
    if text.trim() == "max" {
        Ok(None)
    } else {
        unsigned(text, path, "memory limit").map(Some)
    }
}

fn quota_workers(quota: u64, period: u64) -> u32 {
    let workers = quota / period + u64::from(!quota.is_multiple_of(period));
    u32::try_from(workers).unwrap_or(u32::MAX).max(1)
}

fn positive(text: &str, path: &Path, name: &str) -> Result<u64, TopologyError> {
    let value = unsigned(text, path, name)?;
    if value == 0 {
        Err(parse_error(path, format!("{name} must be positive")))
    } else {
        Ok(value)
    }
}

fn unsigned(text: &str, path: &Path, name: &str) -> Result<u64, TopologyError> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(parse_error(path, format!("invalid {name}")));
    }
    text.parse()
        .map_err(|_| parse_error(path, format!("{name} exceeds u64")))
}

fn minimize<T: Ord + Copy>(current: &mut Option<T>, value: Option<T>) {
    if let Some(value) = value {
        *current = Some(current.map_or(value, |current| current.min(value)));
    }
}

fn optional(
    fs: &dyn FileSystem,
    path: &Path,
    limit: usize,
) -> Result<Option<String>, TopologyError> {
    match fs.read_to_string_bounded(path, limit) {
        Ok(text) => Ok(Some(text)),
        Err(FsError::NotFound { .. }) => Ok(None),
        Err(error) => Err(parse_error(path, error.to_string())),
    }
}

fn parse_error(path: &Path, detail: impl Into<String>) -> TopologyError {
    TopologyError::Parse {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::VirtualFs;

    fn v2(group: &str, root: &str, mountpoint: &str) -> VirtualFs {
        let fs = VirtualFs::new();
        fs.insert(MEMBERSHIP, format!("0::{group}\n"));
        fs.insert(
            MOUNTS,
            format!("31 20 0:27 {root} {mountpoint} rw,nosuid shared:4 - cgroup2 cgroup rw\n"),
        );
        fs
    }

    fn v1() -> VirtualFs {
        let fs = VirtualFs::new();
        fs.insert(
            MEMBERSHIP,
            "2:cpu,cpuacct:/tenant/job\n3:memory:/tenant/job\n4:name=systemd:/user.slice\n",
        );
        fs.insert(MOUNTS, "31 20 0:27 /tenant /controllers/cpu rw - cgroup cgroup rw,cpu,cpuacct\n32 20 0:28 /tenant /controllers/memory rw - cgroup cgroup rw,memory\n");
        fs
    }

    #[test]
    fn absent_optional_surfaces_leave_limits_unknown() {
        assert_eq!(read(&VirtualFs::new()).unwrap(), LinuxLimits::default());
        let fs = VirtualFs::new();
        fs.insert(MEMBERSHIP, "0::/\n");
        fs.insert(THREAD_STATUS, "Name:\tfmn\nCpus_allowed_list:\t2-3,7\n");
        assert_eq!(read(&fs).unwrap().allowed_cpus, Some(vec![2, 3, 7]));
        assert_eq!(read(&fs).unwrap().cpu_quota, None);
    }

    #[test]
    fn affinity_prefers_current_thread_and_falls_back_when_unavailable() {
        let fs = VirtualFs::new();
        fs.insert(PROCESS_STATUS, "Cpus_allowed_list:\t0-7\n");
        assert_eq!(read(&fs).unwrap().allowed_cpus, Some((0..8).collect()));
        fs.insert(THREAD_STATUS, "Cpus_allowed_list:\t3,5-6\n");
        assert_eq!(read(&fs).unwrap().allowed_cpus, Some(vec![3, 5, 6]));
        fs.insert(THREAD_STATUS, "Name:\tfmn\n");
        assert_eq!(read(&fs).unwrap().allowed_cpus, Some((0..8).collect()));
    }

    #[test]
    fn malformed_affinity_is_not_replaced_by_a_broader_process_mask() {
        for status in [
            "Cpus_allowed_list:\n",
            "Cpus_allowed_list:\t2-1\n",
            "Cpus_allowed_list:\t0-4294967295\n",
            "Cpus_allowed_list:\t1\nCpus_allowed_list:\t2\n",
        ] {
            let fs = VirtualFs::new();
            fs.insert(THREAD_STATUS, status);
            fs.insert(PROCESS_STATUS, "Cpus_allowed_list:\t0-7\n");
            assert!(
                matches!(read(&fs), Err(TopologyError::Parse { path, .. }) if path == Path::new(THREAD_STATUS))
            );
        }
    }

    #[test]
    fn v2_ancestor_limits_apply_beneath_an_unlimited_leaf() {
        let fs = v2("/tenant/team/job", "/tenant", "/controllers/unified");
        fs.insert("/controllers/unified/team/job/cpu.max", "max 100000\n");
        fs.insert("/controllers/unified/team/job/memory.max", "max\n");
        fs.insert("/controllers/unified/team/cpu.max", "150001 100000\n");
        fs.insert("/controllers/unified/team/memory.max", "8388608\n");
        fs.insert("/controllers/unified/cpu.max", "300000 100000\n");
        fs.insert("/controllers/unified/memory.max", "16777216\n");
        fs.insert("/controllers/unified/team/job/memory.high", "4096\n");
        fs.insert("/controllers/cpu.max", "1 100000\n");
        fs.insert("/controllers/memory.max", "0\n");
        let limits = read(&fs).unwrap();
        assert_eq!(limits.cpu_quota, Some(2));
        assert_eq!(limits.memory_bytes, Some(8388608));
    }

    #[test]
    fn v1_separate_mounts_follow_parent_cpu_and_memory_limits() {
        let fs = v1();
        fs.insert("/controllers/cpu/job/cpu.cfs_quota_us", "-1\n");
        fs.insert("/controllers/cpu/job/cpu.cfs_period_us", "100000\n");
        fs.insert("/controllers/cpu/cpu.cfs_quota_us", "50000\n");
        fs.insert("/controllers/cpu/cpu.cfs_period_us", "100000\n");
        fs.insert(
            "/controllers/memory/job/memory.limit_in_bytes",
            "9223372036854771712\n",
        );
        fs.insert("/controllers/memory/memory.limit_in_bytes", "1048576\n");
        fs.insert("/controllers/memory/job/memory.soft_limit_in_bytes", "0\n");
        let limits = read(&fs).unwrap();
        assert_eq!(limits.cpu_quota, Some(1));
        assert_eq!(limits.memory_bytes, Some(1048576));
    }

    #[test]
    fn v2_threaded_cpu_membership_overrides_the_leaders_quota() {
        let fs = v2("/domain/leader", "/", "/controllers");
        fs.insert(THREAD_MEMBERSHIP, "0::/domain/worker\n");
        fs.insert("/controllers/domain/leader/cpu.max", "400000 100000\n");
        fs.insert("/controllers/domain/worker/cpu.max", "50000 100000\n");
        fs.insert("/controllers/domain/memory.max", "8388608\n");
        let limits = read(&fs).unwrap();
        assert_eq!(limits.cpu_quota, Some(1));
        assert_eq!(limits.memory_bytes, Some(8388608));
    }

    #[test]
    fn v1_threaded_cpu_membership_preserves_process_memory_accounting() {
        let fs = v1();
        fs.insert(
            THREAD_MEMBERSHIP,
            "2:cpu,cpuacct:/tenant/worker\n3:memory:/tenant/worker\n",
        );
        fs.insert("/controllers/cpu/job/cpu.cfs_quota_us", "400000\n");
        fs.insert("/controllers/cpu/job/cpu.cfs_period_us", "100000\n");
        fs.insert("/controllers/cpu/worker/cpu.cfs_quota_us", "50000\n");
        fs.insert("/controllers/cpu/worker/cpu.cfs_period_us", "100000\n");
        fs.insert("/controllers/memory/job/memory.limit_in_bytes", "8388608\n");
        fs.insert(
            "/controllers/memory/worker/memory.limit_in_bytes",
            "1048576\n",
        );
        let limits = read(&fs).unwrap();
        assert_eq!(limits.cpu_quota, Some(1));
        assert_eq!(limits.memory_bytes, Some(8388608));
    }

    #[test]
    fn v1_memory_inheritance_stops_at_a_nonhierarchical_parent() {
        let fs = v1();
        fs.insert("/controllers/memory/job/memory.limit_in_bytes", "8388608\n");
        fs.insert("/controllers/memory/memory.limit_in_bytes", "1048576\n");
        fs.insert("/controllers/memory/memory.use_hierarchy", "0\n");
        assert_eq!(read(&fs).unwrap().memory_bytes, Some(8388608));
        fs.insert("/controllers/memory/memory.use_hierarchy", "1\n");
        assert_eq!(read(&fs).unwrap().memory_bytes, Some(1048576));
        fs.insert("/controllers/memory/memory.use_hierarchy", "invalid\n");
        assert!(
            matches!(read(&fs), Err(TopologyError::Parse { path, .. }) if path == Path::new("/controllers/memory/memory.use_hierarchy"))
        );
    }

    #[test]
    fn fractional_quota_rounds_up_without_overflow_and_zero_memory_is_real() {
        let fs = v2("/job", "/", "/controllers");
        for (quota, expected) in [(1, 1), (100000, 1), (100001, 2), (u64::MAX, u32::MAX)] {
            fs.insert("/controllers/job/cpu.max", format!("{quota} 100000\n"));
            assert_eq!(read(&fs).unwrap().cpu_quota, Some(expected));
        }
        fs.insert("/controllers/job/cpu.max", format!("{} 1\n", u64::MAX));
        fs.insert("/controllers/job/memory.max", "0\n");
        let limits = read(&fs).unwrap();
        assert_eq!(limits.cpu_quota, Some(u32::MAX));
        assert_eq!(limits.memory_bytes, Some(0));
    }

    #[test]
    fn mount_root_stripping_is_component_aware_and_escaped_paths_are_decoded() {
        let fs = v2(
            "/tenant space/job",
            "/tenant\\040space",
            "/controllers\\040space\\134mount",
        );
        fs.insert("/controllers space\\mount/job/cpu.max", "200000 100000\n");
        assert_eq!(read(&fs).unwrap().cpu_quota, Some(2));
        let fs = v2("/tenant-other/job", "/tenant", "/controllers");
        fs.insert("/controllers/-other/job/cpu.max", "100000 100000\n");
        assert_eq!(read(&fs).unwrap().cpu_quota, None);
    }

    #[test]
    fn widest_matching_mount_retains_restrictive_ancestors() {
        let fs = v2("/tenant/job", "/tenant", "/narrow");
        fs.insert(MOUNTS, "31 20 0:27 /tenant /narrow rw - cgroup2 cgroup rw\n32 20 0:27 / /wide rw - cgroup2 cgroup rw\n");
        fs.insert("/narrow/job/cpu.max", "max 100000\n");
        fs.insert("/wide/tenant/job/cpu.max", "max 100000\n");
        fs.insert("/wide/cpu.max", "100000 100000\n");
        assert_eq!(read(&fs).unwrap().cpu_quota, Some(1));
    }

    #[test]
    fn namespaces_with_hidden_ancestry_do_not_guess_controller_paths() {
        for (membership, root) in [("/job", "/.."), ("/job", "/host/tenant"), ("/../job", "/")] {
            let fs = v2(membership, root, "/controllers");
            fs.insert(THREAD_STATUS, "Cpus_allowed_list:\t2-3\n");
            fs.insert("/controllers/job/cpu.max", "100000 100000\n");
            let limits = read(&fs).unwrap();
            assert_eq!(limits.allowed_cpus, Some(vec![2, 3]));
            assert_eq!(limits.cpu_quota, None);
        }
    }

    #[test]
    fn malformed_present_controller_values_report_the_exact_file() {
        let fs = v2("/job", "/", "/controllers");
        for text in [
            "",
            "max",
            "1 0",
            "0 100000",
            "-1 100000",
            "max 1 extra",
            "18446744073709551616 1",
        ] {
            fs.insert("/controllers/job/cpu.max", text);
            assert!(
                matches!(read(&fs), Err(TopologyError::Parse { path, .. }) if path == Path::new("/controllers/job/cpu.max"))
            );
        }
        fs.insert("/controllers/job/cpu.max", "max 100000");
        for text in ["", "-1", "18446744073709551616", "1 2"] {
            fs.insert("/controllers/job/memory.max", text);
            assert!(
                matches!(read(&fs), Err(TopologyError::Parse { path, .. }) if path == Path::new("/controllers/job/memory.max"))
            );
        }
        let fs = v1();
        fs.insert("/controllers/cpu/job/cpu.cfs_quota_us", "100000");
        assert!(
            matches!(read(&fs), Err(TopologyError::Parse { path, .. }) if path == Path::new("/controllers/cpu/job/cpu.cfs_period_us"))
        );
    }

    #[test]
    fn malformed_discovery_and_parent_traversal_are_diagnostic() {
        for membership in [
            "broken",
            "0::relative",
            "0::/tenant/../escape",
            "0:cpu:/job",
            "1::/job",
            "0::/job\n0::/other",
            "1:cpu,cpu:/job",
        ] {
            let fs = v2("/job", "/", "/controllers");
            fs.insert(MEMBERSHIP, membership);
            assert!(
                matches!(read(&fs), Err(TopologyError::Parse { path, .. }) if path == Path::new(MEMBERSHIP))
            );
        }
        for mountpoint in ["relative", "/controllers/../escape", "/bad\\999escape"] {
            let fs = v2("/job", "/", mountpoint);
            assert!(
                matches!(read(&fs), Err(TopologyError::Parse { path, .. }) if path == Path::new(MOUNTS))
            );
        }
    }

    #[test]
    fn discovery_reads_and_ancestry_are_bounded() {
        for (path, limit) in [
            (THREAD_STATUS, MAX_PROC_STATUS_BYTES),
            (MEMBERSHIP, MAX_MEMBERSHIP_BYTES),
            (THREAD_MEMBERSHIP, MAX_MEMBERSHIP_BYTES),
            (MOUNTS, MAX_MOUNTINFO_BYTES),
        ] {
            let fs = v2("/job", "/", "/controllers");
            fs.insert(path, vec![b'x'; limit + 1]);
            assert!(
                matches!(read(&fs), Err(TopologyError::Parse { path: actual, .. }) if actual == Path::new(path))
            );
        }
        let deep = format!("/{}", vec!["nested"; MAX_PATH_COMPONENTS + 1].join("/"));
        assert!(read(&v2(&deep, "/", "/controllers")).is_err());
        let fs = v2("/job", "/", "/controllers");
        fs.insert(
            "/controllers/job/cpu.max",
            vec![b'1'; MAX_TOPOLOGY_SCALAR_BYTES + 1],
        );
        assert!(
            matches!(read(&fs), Err(TopologyError::Parse { path, .. }) if path == Path::new("/controllers/job/cpu.max"))
        );
    }
}
