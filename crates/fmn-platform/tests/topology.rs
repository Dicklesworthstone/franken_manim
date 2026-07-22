//! HardwareTopology introspection tests (fm-x68 acceptance):
//! synthetic sysfs trees (x86 SMT+CCD, aarch64 big.LITTLE) through the
//! filesystem capability, Windows processor-group handling against synthetic
//! topologies, and the real-machine snapshot fixture flow.
//!
//! To (re)record the committed snapshot of the machine running the tests:
//! `REGEN_TOPOLOGY=1 cargo test -p fmn-platform --test topology`, then
//! commit `fixtures/topology_<platform>.snapshot.txt`.

use fmn_platform::fs::VirtualFs;
use fmn_platform::topology::{HardwareTopology, PerfClass, TopologyError};
use std::path::PathBuf;

fn load_tree(name: &str) -> VirtualFs {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    let manifest = std::fs::read_to_string(path).expect("fixture manifest present");
    let fs = VirtualFs::new();
    fs.load_manifest(&manifest);
    fs
}

#[test]
fn x86_smt_ccd_tree_parses_fully() {
    let fs = load_tree("sysfs_x86_smt_ccd.tsv");
    let t = HardwareTopology::detect_linux(&fs).expect("detect");

    assert_eq!(t.logical_cores(), 16);
    assert_eq!(t.physical_cores, 8);
    assert_eq!(t.packages, 1);
    assert!(t.smt_active());
    // Uniform frequencies: every CPU is Performance.
    assert!(t.cpus.iter().all(|c| c.class == PerfClass::Performance));
    // SMT pairs share an L2 (8 pair domains); two 4-core L3 domains (CCDs).
    assert_eq!(t.l2_domains.len(), 8);
    assert_eq!(t.l2_domains[0].cpus, vec![0, 8]);
    assert_eq!(t.l2_domains[0].size_bytes, Some(1024 * 1024));
    assert_eq!(t.l3_domains.len(), 2);
    assert_eq!(t.l3_domains[0].cpus, vec![0, 1, 2, 3, 8, 9, 10, 11]);
    assert_eq!(t.l3_domains[1].cpus, vec![4, 5, 6, 7, 12, 13, 14, 15]);
    assert_eq!(t.l3_domains[0].size_bytes, Some(16384 * 1024));
    // Two NUMA nodes matching the L3 split.
    assert_eq!(t.numa_nodes.len(), 2);
    assert_eq!(t.numa_nodes[0].cpus, t.l3_domains[0].cpus);
    // 16 CPUs fit one processor group.
    assert_eq!(t.processor_groups.len(), 1);
    assert_eq!(t.processor_groups[0].cpus.len(), 16);
    assert_eq!(t.total_memory_bytes, Some(32_768_000 * 1024));
}

#[test]
fn aarch64_biglittle_tree_derives_perf_classes() {
    let fs = load_tree("sysfs_aarch64_biglittle.tsv");
    let t = HardwareTopology::detect_linux(&fs).expect("detect");

    assert_eq!(t.logical_cores(), 8);
    assert_eq!(t.physical_cores, 8);
    assert!(!t.smt_active());
    // Capacity 512 → Efficiency (cpu0-3); capacity 1024 → Performance (cpu4-7).
    for c in &t.cpus {
        let expected = if c.id < 4 {
            PerfClass::Efficiency
        } else {
            PerfClass::Performance
        };
        assert_eq!(c.class, expected, "cpu{}", c.id);
        assert_eq!(c.capacity, Some(if c.id < 4 { 512 } else { 1024 }));
    }
    // Two L2 cluster domains, one DSU L3, implicit single NUMA node.
    assert_eq!(t.l2_domains.len(), 2);
    assert_eq!(t.l2_domains[0].cpus, vec![0, 1, 2, 3]);
    assert_eq!(t.l2_domains[1].cpus, vec![4, 5, 6, 7]);
    assert_eq!(t.l3_domains.len(), 1);
    assert_eq!(t.l3_domains[0].cpus.len(), 8);
    assert_eq!(t.numa_nodes.len(), 1);
    assert_eq!(t.total_memory_bytes, Some(8_000_000 * 1024));
}

#[test]
fn windows_groups_split_above_64_and_reject_oversize() {
    // A 96-logical machine models as two groups: 64 + 32.
    let t = HardwareTopology::from_group_sizes(&[64, 32]).expect("96-cpu model");
    assert_eq!(t.logical_cores(), 96);
    assert_eq!(t.processor_groups.len(), 2);
    assert_eq!(t.processor_groups[0].cpus.len(), 64);
    assert_eq!(t.processor_groups[1].cpus.len(), 32);
    assert_eq!(t.processor_groups[1].cpus[0], 64);

    // A single group above 64 violates the Windows invariant.
    assert!(matches!(
        HardwareTopology::from_group_sizes(&[65]),
        Err(TopologyError::Invalid { .. })
    ));
    assert!(matches!(
        HardwareTopology::from_group_sizes(&[]),
        Err(TopologyError::Invalid { .. })
    ));

    // The fallback constructor applies the same split: 128 logical → 2×64,
    // so even a topology-blind host cannot produce an oversized group.
    let f = HardwareTopology::fallback(128);
    assert_eq!(f.processor_groups.len(), 2);
    assert!(f.processor_groups.iter().all(|g| g.cpus.len() <= 64));
}

#[test]
fn degraded_tree_still_detects() {
    // Only the online list: everything optional missing. Detection succeeds
    // with sane defaults (core_id = cpu id, package 0, no domains).
    let fs = VirtualFs::new();
    fs.insert("/sys/devices/system/cpu/online", b"0-3\n".to_vec());
    let t = HardwareTopology::detect_linux(&fs).expect("degraded detect");
    assert_eq!(t.logical_cores(), 4);
    assert_eq!(t.physical_cores, 4);
    assert!(t.l3_domains.is_empty());
    assert_eq!(t.numa_nodes.len(), 1);
    assert_eq!(t.total_memory_bytes, None);

    // No online list at all: a named error, not a guess.
    let empty = VirtualFs::new();
    assert!(matches!(
        HardwareTopology::detect_linux(&empty),
        Err(TopologyError::Missing { .. })
    ));
}

#[test]
fn snapshot_text_is_deterministic_and_versioned() {
    let fs = load_tree("sysfs_x86_smt_ccd.tsv");
    let t = HardwareTopology::detect_linux(&fs).expect("detect");
    let a = t.snapshot_text();
    assert_eq!(a, t.snapshot_text());
    assert!(a.starts_with("# fmn hardware-topology snapshot v1\n"));
    assert!(a.contains("logical_cores\t16"));
    assert!(a.contains("l3\tsize\t16777216\tcpus\t0-3,8-11"));
}

/// The real machine: detection succeeds and the committed per-platform
/// snapshot fixture exists for this OS/arch (recorded, not asserted — the
/// snapshot is a fixture of *a* machine of this platform, and CI hosts
/// differ in core count).
#[test]
#[cfg(target_os = "linux")]
fn real_machine_detects_and_snapshot_recorded() {
    let t = HardwareTopology::current();
    assert!(t.logical_cores() >= 1);
    assert!(t.physical_cores >= 1 && t.physical_cores <= t.logical_cores());
    assert!(t.processor_groups.iter().all(|g| g.cpus.len() <= 64));

    let name = format!(
        "topology_{}-{}.snapshot.txt",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    if std::env::var_os("REGEN_TOPOLOGY").is_some() {
        std::fs::write(&path, t.snapshot_text()).expect("write snapshot");
        eprintln!("recorded {}", path.display());
        return;
    }
    // x86-64 CI is the committed baseline; other platforms record theirs
    // the first time the suite runs there (see the module doc).
    if std::env::consts::ARCH == "x86_64" {
        let text = std::fs::read_to_string(&path)
            .expect("committed topology snapshot for linux-x86_64; record with REGEN_TOPOLOGY=1");
        assert!(text.starts_with("# fmn hardware-topology snapshot v1\n"));
    }
}
