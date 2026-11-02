//! Process trees and their memory. macOS: `phys_footprint` (Activity Monitor's "Memory"), summed
//! over the process and its descendants; Linux: PSS from `/proc/<pid>/smaps_rollup`.

/// Bytes used by `pid` and all its descendants (0 if it is gone).
pub fn tree_memory(pid: u32) -> u64 {
    let mut total = 0;
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        total += platform::memory(p);
        stack.extend(platform::children(p));
    }
    total
}

#[cfg(target_os = "macos")]
mod platform {
    use std::mem::MaybeUninit;

    pub fn memory(pid: u32) -> u64 {
        let mut info = MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: the buffer is a rusage_info_v2, which is what RUSAGE_INFO_V2 writes.
        let rc = unsafe {
            libc::proc_pid_rusage(pid as i32, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast())
        };
        // SAFETY: zero-initialized, and fully written on success.
        if rc == 0 {
            unsafe { info.assume_init() }.ri_phys_footprint
        } else {
            0
        }
    }

    pub fn children(pid: u32) -> Vec<u32> {
        let mut buf = vec![0 as libc::pid_t; 4096];
        let size = (buf.len() * size_of::<libc::pid_t>()) as i32;
        // SAFETY: buf has room for `size` bytes; the call returns how many pids it wrote.
        let n = unsafe { libc::proc_listchildpids(pid as i32, buf.as_mut_ptr().cast(), size) };
        buf.truncate(n.max(0) as usize);
        buf.into_iter().map(|p| p as u32).collect()
    }
}

#[cfg(target_os = "linux")]
mod platform {
    pub fn memory(pid: u32) -> u64 {
        let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")) else {
            return 0;
        };
        let kb = text
            .lines()
            .find_map(|l| l.strip_prefix("Pss:"))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok());
        kb.unwrap_or(0) * 1024
    }

    pub fn children(pid: u32) -> Vec<u32> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return vec![];
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|p| {
                // Field 4 of /proc/<p>/stat is the parent pid; field 2 (comm) may hold spaces, so split after ')'.
                let stat = std::fs::read_to_string(format!("/proc/{p}/stat")).unwrap_or_default();
                stat.rsplit_once(')')
                    .and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok())
                    == Some(pid)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_self_and_children() {
        let me = tree_memory(std::process::id());
        assert!(me > 1_000_000, "{me}");
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(platform::children(std::process::id()).contains(&child.id()));
        assert!(tree_memory(std::process::id()) > me);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(tree_memory(u32::MAX - 1), 0, "gone pid");
    }
}
