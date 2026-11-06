//! Owned agent processes, on Tokio's command and child.
pub use std::process::Stdio;
pub use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub(crate) fn signal_target(child: &Child) -> Option<i32> {
    let pid = child.id()? as i32;
    // ACP children lead a private group; other harnesses retain pid signaling.
    // SAFETY: getpgid only inspects the owned, unreaped child.
    Some(if unsafe { libc::getpgid(pid) } == pid {
        -pid
    } else {
        pid
    })
}

/// One process a descendant snapshot found: its pid and, where the process
/// table reports it, its start time, which tells it apart from an unrelated
/// process that later inherits the pid. Each snapshot carries its own
/// identities to [`terminate_tree`]: runs never share or overwrite them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tracked {
    pub(crate) pid: i32,
    start: Option<u64>,
}

impl Tracked {
    /// `pid` as the process table describes it right now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn now(pid: i32) -> Self {
        Self {
            pid,
            start: proc_table::start_time(pid),
        }
    }
}

/// Every live descendant of `root` — children, grandchildren, … — from one
/// process-table snapshot. Agent CLIs start each shell in its OWN process
/// group (Claude's background Bash, Codex's exec sessions), so neither a
/// signal to the agent nor to its group reaches them, and a background
/// shell outlives its agent (verified live, Claude 2.1.286: its own task
/// stop and SIGTERM both leave the shell running). Snapshot while the agent
/// is alive: once it exits, its orphans are reparented and untraceable.
///
/// Each descendant carries its start time, so [`terminate_tree`] can tell
/// the process it found from an unrelated one that inherited the pid after
/// it exited.
pub(crate) async fn descendants(root: u32) -> Vec<Tracked> {
    let mut table = tokio::task::spawn_blocking(proc_table::read)
        .await
        .unwrap_or_default();
    if table.is_empty() {
        // No /proc or libproc view (or a sandbox hid it): ask ps.
        table = ps_table().await;
    }
    tree_of(root as i32, &table)
        .into_iter()
        .map(|pid| Tracked {
            pid,
            start: table
                .iter()
                .find(|entry| entry.pid == pid)
                .and_then(|entry| entry.start),
        })
        .collect()
}

/// One process-table row; `start` identifies the process across pid reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcEntry {
    pid: i32,
    ppid: i32,
    start: Option<u64>,
}

fn tree_of(root: i32, table: &[ProcEntry]) -> Vec<i32> {
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for entry in table {
            if entry.ppid == parent && entry.pid > 1 && !found.contains(&entry.pid) {
                found.push(entry.pid);
                frontier.push(entry.pid);
            }
        }
    }
    found
}

async fn ps_table() -> Vec<ProcEntry> {
    let Ok(out) = tokio::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
    else {
        tracing::warn!("no process table (/proc or ps): agent descendants cannot be traced");
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace().map(str::parse::<i32>);
            let (pid, ppid) = (cols.next()?.ok()?, cols.next()?.ok()?);
            Some(ProcEntry {
                pid,
                ppid,
                start: proc_table::start_time(pid),
            })
        })
        .collect()
}

/// Native process-table readers: Linux's /proc (slim images often ship no
/// `ps`) and macOS's libproc. Both report each process's start time.
mod proc_table {
    use super::ProcEntry;

    #[cfg(target_os = "linux")]
    pub(super) fn read() -> Vec<ProcEntry> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(|pid| {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                let (ppid, start) = parse_stat(&stat)?;
                Some(ProcEntry {
                    pid,
                    ppid,
                    start: Some(start),
                })
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    pub(super) fn start_time(pid: i32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_stat(&stat).map(|(_, start)| start)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn read() -> Vec<ProcEntry> {
        // SAFETY: a null buffer asks only for the current pid count.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return Vec::new();
        }
        // Headroom for processes started between the two calls.
        let mut pids = vec![0 as libc::pid_t; count as usize + 64];
        let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
        // SAFETY: `bytes` is the buffer's true size.
        let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if filled <= 0 {
            return Vec::new();
        }
        pids.truncate((filled as usize).min(pids.len()));
        pids.into_iter()
            .filter_map(|pid| {
                let info = bsd_info(pid)?;
                Some(ProcEntry {
                    pid,
                    ppid: info.pbi_ppid as i32,
                    start: Some(start_of(&info)),
                })
            })
            .collect()
    }

    #[cfg(target_os = "macos")]
    pub(super) fn start_time(pid: i32) -> Option<u64> {
        bsd_info(pid).map(|info| start_of(&info))
    }

    #[cfg(target_os = "macos")]
    fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
        // SAFETY: proc_bsdinfo is plain data; proc_pidinfo writes at most
        // `size` bytes into it and reports how many it wrote.
        unsafe {
            let mut info: libc::proc_bsdinfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
            let written = libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            );
            (written == size).then_some(info)
        }
    }

    #[cfg(target_os = "macos")]
    fn start_of(info: &libc::proc_bsdinfo) -> u64 {
        info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn read() -> Vec<ProcEntry> {
        Vec::new()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn start_time(_pid: i32) -> Option<u64> {
        None
    }

    /// `(ppid, starttime)` from a `/proc/<pid>/stat` line: fields 4 and 22,
    /// counted after the parenthesized command name (which may itself hold
    /// spaces and parentheses).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(super) fn parse_stat(stat: &str) -> Option<(i32, u64)> {
        let rest = &stat[stat.rfind(')')? + 1..];
        let fields: Vec<&str> = rest.split_whitespace().collect();
        Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
    }
}

/// Start times recorded by [`descendants`] snapshots, consulted before every
/// signal [`terminate_tree`] sends. The shared drivers pass bare pids
/// between the two, so the identity travels here.
/// Whether `pid` is still the process a snapshot found: same start time when
/// one was recorded, merely alive otherwise.
fn still_same(pid: i32, recorded: Option<u64>) -> bool {
    match recorded {
        Some(start) => proc_table::start_time(pid) == Some(start),
        // SAFETY: signal 0 only checks for existence.
        None => (unsafe { libc::kill(pid, 0) }) == 0,
    }
}

/// Terminate `tracked` processes and the process groups they lead: SIGTERM,
/// then SIGKILL whatever is still running after `grace`. A pid whose process
/// has exited and been reused by another is never signaled.
pub(crate) async fn terminate_tree(tracked: &[Tracked], grace: std::time::Duration) {
    let recorded: Vec<(i32, Option<u64>)> = tracked
        .iter()
        .map(|process| (process.pid, process.start))
        .collect();
    // A group leader's whole group (its own children included); anything
    // else by pid. A leader verified now names its group for as long as the
    // group has members: the kernel never hands out a live group's id.
    let targets: Vec<(i32, Option<u64>)> = recorded
        .iter()
        .filter(|&&(pid, start)| still_same(pid, start))
        .map(|&(pid, start)| {
            // SAFETY: getpgid only inspects; a vanished pid returns -1.
            if unsafe { libc::getpgid(pid) } == pid {
                (-pid, start)
            } else {
                (pid, start)
            }
        })
        .collect();
    // SAFETY: signal 0 only checks for existence.
    let alive = |target: i32| unsafe { libc::kill(target, 0) } == 0;
    // The grace can outlast a process; a plain pid is re-verified before
    // each signal, as it may have been reused in between.
    let current = |&(target, start): &(i32, Option<u64>)| {
        if target < 0 {
            alive(target)
        } else {
            still_same(target, start)
        }
    };
    for target in &targets {
        if current(target) {
            crate::send_signal(&target.0, crate::Signal::Term);
        }
    }
    let deadline = tokio::time::Instant::now() + grace;
    while targets.iter().any(current) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    for target in &targets {
        if current(target) {
            crate::send_signal(&target.0, crate::Signal::Kill);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grandchild in its own process group (how agent CLIs start shells)
    /// survives its parent's death; the descendant snapshot still finds it
    /// and `terminate_tree` ends it.
    #[tokio::test]
    async fn tree_teardown_reaches_shells_in_their_own_process_group() {
        /// Ends the fixture shell even when an assertion fails first.
        struct Reap(i32);
        impl Drop for Reap {
            fn drop(&mut self) {
                // SAFETY: SIGKILL to the fixture's own pid.
                unsafe { libc::kill(self.0, libc::SIGKILL) };
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        // The child reports its pid only once it leads its own group: waiting
        // for it to exist was not enough (it had not called setpgid yet).
        let script = format!(
            "python3 -c 'import os, time; os.setpgid(0, 0); open(\"{}\", \"w\").write(str(os.getpid())); time.sleep(60)' & sleep 60",
            ready.display()
        );
        let mut parent = tokio::process::Command::new("sh")
            .args(["-c", &script])
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let root = parent.id().unwrap();
        let mut shell = None;
        for _ in 0..250 {
            if let Some(pid) = std::fs::read_to_string(&ready)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
            {
                shell = Some(pid);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let shell = shell.expect("the shell leads its own process group");
        let _reap = Reap(shell);
        assert_eq!(unsafe { libc::getpgid(shell) }, shell);
        let tree = descendants(root).await;
        assert!(tree.iter().any(|found| found.pid == shell), "{tree:?}");
        // The parent dies hard (an unresponsive agent's SIGKILL): the shell
        // is orphaned, not killed.
        parent.kill().await.unwrap();
        assert_eq!(unsafe { libc::kill(shell, 0) }, 0, "orphan survives");
        terminate_tree(&tree, std::time::Duration::from_secs(2)).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_ne!(unsafe { libc::kill(shell, 0) }, 0, "orphan ended");
    }

    #[test]
    fn proc_stat_fields_survive_hostile_command_names() {
        // Field 22 (starttime) and 4 (ppid), with a comm holding ") (".
        let stat = "4242 (evil) (name) S 17 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10";
        assert_eq!(proc_table::parse_stat(stat), Some((17, 987654)));
        assert_eq!(proc_table::parse_stat("4242 (truncated) S 17"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_table_finds_children_with_start_times() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap() as i32;
        let table = proc_table::read();
        let me = std::process::id() as i32;
        let entry = table
            .iter()
            .find(|entry| entry.pid == pid)
            .expect("child in the native table");
        assert_eq!(entry.ppid, me);
        assert!(entry.start.is_some());
        assert_eq!(proc_table::start_time(pid), entry.start);
        assert!(tree_of(me, &table).contains(&pid));
        child.kill().await.unwrap();
    }

    /// A pid whose recorded start time no longer matches belongs to some
    /// other process now: teardown must leave it alone.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn teardown_skips_a_pid_that_was_reused() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap() as i32;
        let start = proc_table::start_time(pid).unwrap();
        let impostor = Tracked {
            pid,
            start: Some(start + 1),
        };
        terminate_tree(&[impostor], std::time::Duration::from_millis(200)).await;
        assert!(
            child.try_wait().unwrap().is_none(),
            "an impostor pid was signaled"
        );
        terminate_tree(&[Tracked::now(pid)], std::time::Duration::from_millis(200)).await;
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .expect("the verified process is ended")
            .unwrap();
        assert!(!status.success());
    }

    #[tokio::test]
    async fn unix_launch_retains_tokio_types_and_output() {
        let mut command: tokio::process::Command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--list")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let child: tokio::process::Child = command.spawn().unwrap();
        let result = child.wait_with_output().await.unwrap();
        assert!(result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stdout)
                .contains("unix_launch_retains_tokio_types_and_output")
        );
        let result = command.output().await.unwrap();
        assert!(result.status.success());
        assert!(!result.stdout.is_empty());
    }
}

pub(crate) mod owned;
pub(crate) mod reaper;
