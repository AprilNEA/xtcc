//! Read-only process discovery. This module neither acquires task ports nor elevates privileges.

use anyhow::{bail, ensure, Context, Result};
use std::{io, mem::size_of, ptr};

pub const TCCD_PATH: &str = "/System/Library/PrivateFrameworks/TCC.framework/Support/tccd";
// SDK sys/proc_info.h: PROC_PIDPATHINFO_MAXSIZE = 4 * MAXPATHLEN.
const PID_PATH_CAPACITY: usize = 4096;
const MAX_PID_CAPACITY: usize = 1_048_576;

fn process_path(pid: i32) -> Result<Vec<u8>> {
    ensure!(pid > 0, "invalid pid: {pid}");
    let mut buffer = [0u8; PID_PATH_CAPACITY];
    // SAFETY: The buffer is writable and its length equals the capacity passed to SDK proc_pidpath.
    let count = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if count <= 0 {
        return Err(io::Error::last_os_error()).with_context(|| format!("proc_pidpath({pid})"));
    }
    ensure!(
        count as usize <= buffer.len(),
        "proc_pidpath returned invalid length"
    );
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .context("unterminated process path")?;
    ensure!(end != 0, "empty process path");
    Ok(buffer[..end].to_vec())
}

pub fn verify_tccd(pid: i32) -> Result<()> {
    let path = process_path(pid)?;
    ensure!(
        path == TCCD_PATH.as_bytes(),
        "target_is_not_tccd: pid {pid}"
    );
    Ok(())
}

pub fn live_tccd() -> Result<Vec<i32>> {
    // SAFETY: The SDK allows a null pointer and zero capacity to query the current PID count.
    let count = unsafe { libc::proc_listallpids(ptr::null_mut(), 0) };
    if count <= 0 {
        return Err(io::Error::last_os_error()).context("proc_listallpids size query");
    }
    let mut capacity = (count as usize)
        .checked_add(64)
        .context("pid capacity overflow")?;
    // Bounded retries handle new processes during enumeration; do not return a partial snapshot.
    for _ in 0..4 {
        ensure!(
            capacity <= MAX_PID_CAPACITY,
            "process list exceeds capacity limit"
        );
        let mut pids = vec![0i32; capacity];
        let bytes = capacity
            .checked_mul(size_of::<i32>())
            .context("pid buffer overflow")?;
        let bytes = i32::try_from(bytes).context("pid buffer exceeds SDK limit")?;
        // SAFETY: Vec provides an initialized, properly aligned pid_t array; bytes is its actual capacity.
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if count < 0 {
            return Err(io::Error::last_os_error()).context("proc_listallpids");
        }
        ensure!(count > 0, "proc_listallpids returned an empty snapshot");
        if count as usize >= capacity {
            capacity = capacity.checked_mul(2).context("pid capacity overflow")?;
            continue;
        }
        pids.truncate(count as usize);
        let mut found: Vec<i32> = pids
            .into_iter()
            .filter(|pid| {
                // Exited or unreadable processes are not verified targets.
                *pid > 0 && process_path(*pid).is_ok_and(|path| path == TCCD_PATH.as_bytes())
            })
            .collect();
        found.sort_unstable();
        found.dedup();
        return Ok(found);
    }
    bail!("process list changed during all enumeration attempts")
}

pub fn require_root() -> Result<()> {
    // SAFETY: geteuid takes no pointers and only reads this process's effective user ID.
    let uid = unsafe { libc::geteuid() };
    ensure!(uid == 0, "root_required: effective uid {uid}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_nonpositive_pids_without_syscall() {
        assert!(verify_tccd(0).is_err());
        assert!(verify_tccd(-1).is_err());
    }

    #[test]
    fn only_current_process_path_is_read_in_test() {
        let pid = std::process::id() as i32;
        assert!(!process_path(pid).unwrap().is_empty());
        assert!(verify_tccd(pid).is_err());
        assert_eq!(
            TCCD_PATH,
            "/System/Library/PrivateFrameworks/TCC.framework/Support/tccd"
        );
    }

    #[test]
    fn root_check_never_changes_identity() {
        // SAFETY: geteuid only reads this process's identity and has no pointer preconditions.
        let before = unsafe { libc::geteuid() };
        assert_eq!(require_root().is_ok(), before == 0);
        // SAFETY: As above; the test verifies that require_root did not change the identity.
        assert_eq!(unsafe { libc::geteuid() }, before);
    }
}
