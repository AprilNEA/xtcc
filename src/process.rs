//! 只读进程发现。此模块不获取 task port，也不执行提权。

use anyhow::{bail, ensure, Context, Result};
use std::{io, mem::size_of, ptr};

pub const TCCD_PATH: &str = "/System/Library/PrivateFrameworks/TCC.framework/Support/tccd";
// SDK sys/proc_info.h：PROC_PIDPATHINFO_MAXSIZE = 4 * MAXPATHLEN。
const PID_PATH_CAPACITY: usize = 4096;
const MAX_PID_CAPACITY: usize = 1_048_576;

fn process_path(pid: i32) -> Result<Vec<u8>> {
    ensure!(pid > 0, "invalid pid: {pid}");
    let mut buffer = [0u8; PID_PATH_CAPACITY];
    // SAFETY: 缓冲区可写且长度等于传给 SDK proc_pidpath 的容量。
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
    // SAFETY: SDK 允许空指针和零容量查询当前 PID 数量。
    let count = unsafe { libc::proc_listallpids(ptr::null_mut(), 0) };
    if count <= 0 {
        return Err(io::Error::last_os_error()).context("proc_listallpids size query");
    }
    let mut capacity = (count as usize)
        .checked_add(64)
        .context("pid capacity overflow")?;
    // 有限重试处理枚举期间新增的进程；无法取得完整快照时不报告部分结果。
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
        // SAFETY: Vec 提供已初始化、正确对齐的 pid_t 数组，bytes 是其实际容量。
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
                // 退出或不可读的进程不是已验证目标。
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
    // SAFETY: geteuid 无指针参数，只读取本进程的有效用户 ID。
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
        // SAFETY: geteuid 只读取本进程身份，无前提指针。
        let before = unsafe { libc::geteuid() };
        assert_eq!(require_root().is_ok(), before == 0);
        // SAFETY: 同上；测试确认 require_root 没有改变身份。
        assert_eq!(unsafe { libc::geteuid() }, before);
    }
}
