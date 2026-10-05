//! Memory this process may plan against, and how much parallel work it
//! affords.
//!
//! No `sysinfo`: the workspace has no such dependency, so this reads the
//! platforms that run the planner directly.
//!
//!  * Linux — `/proc/meminfo`'s `MemAvailable`, the kernel's own estimate of
//!    what can be allocated without swapping, lowered to what the process's
//!    control groups leave: a container sees the host's `/proc/meminfo`, and
//!    its own limit is what the out-of-memory killer enforces. A group's
//!    headroom is its limit less its usage, the page cache it could drop
//!    (`inactive_file`) not counted as usage; every group up the hierarchy
//!    bounds it.
//!  * Windows — `GetPhysicallyInstalledSystemMemory`, which reports INSTALLED
//!    memory, not free memory. The number is therefore an over-estimate of
//!    what is really available, and the 70 % share is doing more work here
//!    than on Linux. Accepted because the workstation is where memory is
//!    plentiful, and because the alternative (`GlobalMemoryStatusEx`) buys a
//!    second FFI declaration for a figure that swings with whatever else
//!    happens to be open.
//!  * anything else, or every probe failing — 2 GiB, the smallest machine
//!    this is deployed on. Guessing high on an unknown platform is how a
//!    planner gets OOM-killed; guessing low only makes it slower.

/// Memory this process may plan against, in bytes, as of now.
pub fn usable_memory_bytes() -> u64 {
    const FALLBACK: u64 = 2 * 1024 * 1024 * 1024;
    platform_memory_bytes().filter(|b| *b > 0).unwrap_or(FALLBACK)
}

/// How many jobs of `per_job_bytes` each fit in `available_bytes` beside
/// `fixed_floor_bytes`, capped by the worker count.
///
/// 70 % of `available` is the share a run may claim, which leaves the other
/// 30 % for the allocator's own fragmentation, memory-mapped files, and the
/// rest of a shared machine. Never 0: one job at a time is the smallest unit
/// of work there is, and refusing to run is not better than swapping.
pub fn jobs_that_fit(
    available_bytes: u64,
    fixed_floor_bytes: u64,
    per_job_bytes: u64,
    num_threads: usize,
) -> usize {
    let budget = (available_bytes as f64 * 0.7) as u64;
    let spare = budget.saturating_sub(fixed_floor_bytes);
    let fits = spare / per_job_bytes.max(1);
    (fits as usize).clamp(1, num_threads.max(1))
}

/// The next run of `items` to work on together: as many as the memory free
/// now holds at `cost` bytes each (counted from the 70 % share, as
/// [`jobs_that_fit`]), at most `num_threads`, and at least one. Asked again
/// before each batch, so a batch follows what the earlier ones and the rest
/// of the machine left.
pub fn next_batch<T>(items: &[T], cost: impl Fn(&T) -> u64, num_threads: usize) -> &[T] {
    let budget = (usable_memory_bytes() as f64 * 0.7) as u64;
    let mut used = 0u64;
    let mut n = 0;
    for item in items.iter().take(num_threads.max(1)) {
        used = used.saturating_add(cost(item));
        if n > 0 && used > budget {
            break;
        }
        n += 1;
    }
    &items[..n]
}

#[cfg(target_os = "linux")]
fn platform_memory_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    // MemAvailable first; MemTotal only if the kernel is too old to publish
    // it (pre-3.14), where it is the only figure on offer.
    let system = ["MemAvailable:", "MemTotal:"].iter().find_map(|key| {
        let line = text.lines().find(|l| l.starts_with(key))?;
        Some(line.split_whitespace().nth(1)?.parse::<u64>().ok()? * 1024)
    })?;
    Some(cgroup_headroom().map_or(system, |g| g.min(system)))
}

/// The least any of this process's control groups leaves, where one is
/// limited.
#[cfg(target_os = "linux")]
fn cgroup_headroom() -> Option<u64> {
    let own = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let mut least: Option<u64> = None;
    let mut keep = |h: Option<u64>| {
        if let Some(h) = h {
            least = Some(least.map_or(h, |l| l.min(h)));
        }
    };
    for line in own.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(_), Some(controllers), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if controllers.is_empty() {
            // v2: the one hierarchy, at /sys/fs/cgroup.
            let mut dir = std::path::PathBuf::from("/sys/fs/cgroup");
            dir.push(path.trim_start_matches('/'));
            for d in dir.ancestors().take_while(|d| d.starts_with("/sys/fs/cgroup")) {
                keep(group_headroom(d, "memory.max", "memory.current"));
            }
        } else if controllers.split(',').any(|c| c == "memory") {
            // v1: the memory controller's own hierarchy. Inside a container
            // its directory is mounted as the root; outside, under `path`.
            let root = std::path::Path::new("/sys/fs/cgroup/memory");
            for d in [root.join(path.trim_start_matches('/')), root.to_path_buf()] {
                keep(group_headroom(&d, "memory.limit_in_bytes", "memory.usage_in_bytes"));
            }
        }
    }
    least
}

/// A group's limit less its usage, the page cache it could drop not counted
/// as usage; None where it has no limit or no such files.
#[cfg(target_os = "linux")]
fn group_headroom(dir: &std::path::Path, limit: &str, usage: &str) -> Option<u64> {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    let limit: u64 = read(limit)?.trim().parse().ok()?; // "max" is no limit
    if limit >= 1 << 60 {
        return None; // v1's no limit: the largest page-aligned i64
    }
    let usage: u64 = read(usage)?.trim().parse().ok()?;
    let cache = read("memory.stat")
        .and_then(|s| {
            let line = s
                .lines()
                .find(|l| l.starts_with("inactive_file ") || l.starts_with("total_inactive_file "))?;
            line.split_whitespace().nth(1)?.parse::<u64>().ok()
        })
        .unwrap_or(0);
    Some(limit.saturating_sub(usage.saturating_sub(cache)))
}

#[cfg(windows)]
fn platform_memory_bytes() -> Option<u64> {
    // The only FFI in this workspace. std has no memory-size API, and pulling
    // in a Windows binding crate for one call would put a platform dependency
    // into a crate that is otherwise pure arithmetic. The call writes one u64
    // and returns a BOOL; there is no allocation, no handle and no lifetime
    // involved, which is the whole reason it is the one worth making.
    #[link(name = "kernel32")]
    extern "system" {
        fn GetPhysicallyInstalledSystemMemory(total_kb: *mut u64) -> i32;
    }
    let mut kb: u64 = 0;
    let ok = unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) };
    (ok != 0).then(|| kb * 1024)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_memory_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever the platform probe does, its figure must be a plausible
    /// number of bytes. No lower bound: a busy CI container legitimately has
    /// less than any floor worth asserting.
    #[test]
    fn the_probe_reports_a_plausible_number_of_bytes() {
        let b = usable_memory_bytes();
        assert!(b > 0, "a zero budget would divide the work to nothing");
        assert!(b < 1 << 50, "and 1 PiB is not a machine this runs on, got {b}");
    }

    #[test]
    fn jobs_fit_the_share_beside_the_floor_and_never_none() {
        // 10 GB: a 7 GB share, 1 GB held, 2 GB jobs → 3.
        assert_eq!(jobs_that_fit(10_000_000_000, 1_000_000_000, 2_000_000_000, 16), 3);
        assert_eq!(jobs_that_fit(10_000_000_000, 0, 1, 4), 4, "capped by the workers");
        assert_eq!(jobs_that_fit(1_000, 5_000, 1_000, 8), 1, "one even with nothing spare");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_group_leaves_its_limit_less_what_it_cannot_drop() {
        let dir = std::env::temp_dir().join(format!("planner-cgroup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let put = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        put("memory.max", "2000000000\n");
        put("memory.current", "1500000000\n");
        put("memory.stat", "anon 900000000\nfile 600000000\ninactive_file 500000000\n");
        let h = group_headroom(&dir, "memory.max", "memory.current");
        assert_eq!(h, Some(1_000_000_000), "1.5 GB used, 0.5 GB of it droppable cache");
        put("memory.max", "max\n");
        assert_eq!(group_headroom(&dir, "memory.max", "memory.current"), None, "no limit");
        put("memory.limit_in_bytes", "9223372036854771712\n");
        put("memory.usage_in_bytes", "100\n");
        assert_eq!(
            group_headroom(&dir, "memory.limit_in_bytes", "memory.usage_in_bytes"),
            None,
            "v1's no limit"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_batch_is_at_least_one_and_at_most_the_workers() {
        let items = [1u64, 2, 3, 4, 5];
        assert_eq!(next_batch(&items, |_| 1, 3).len(), 3);
        assert_eq!(next_batch(&items, |_| u64::MAX, 3).len(), 1);
        assert!(next_batch(&[] as &[u64], |_| 1, 3).is_empty());
    }
}
