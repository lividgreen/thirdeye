//! Which process is the agent, and whether it is still running.
//!
//! Some agents never say their session ended: Antigravity has no such hook,
//! and a crashed Claude Code sends no `SessionEnd`. Without one the daemon,
//! and the split it opened, outlive the session until logout. So the hook
//! records the agent's process beside the session's socket, and the daemon
//! watches it.
//!
//! The agent is the hook's first ancestor that is not a shell: every agent
//! runs its hook through one (`sh -c`, Claude's plugin script) or directly.
//! Its start time is recorded with its pid, because a pid is reused and a
//! reused one would keep a dead session alive forever.
//!
//! `/proc` on Linux, the system's process list on Windows — where it matters
//! more, because Claude Code's last hooks there die with it and the end of
//! its session goes unsaid. Elsewhere nothing is recorded, and a daemon that
//! knows no process ends the old way, or not at all.

use serde::{Deserialize, Serialize};

/// The agent's process, as the hook found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcess {
    pub pid: u32,
    /// When it started, in the platform's own terms: clock ticks after boot
    /// (`/proc/<pid>/stat` field 22), or Windows' creation time. Only ever
    /// compared with itself.
    pub start: u64,
}

/// What the system says about one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub ppid: u32,
    pub comm: String,
    pub start: u64,
    /// Exited but not yet reaped: as gone as an agent gets.
    pub zombie: bool,
}

/// Programs an agent runs its hooks through, which are never the agent.
const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "dash",
    "zsh",
    "fish",
    "ksh",
    "mksh",
    "busybox",
    // Windows' own, as its process list names them once `.exe` is dropped.
    "cmd",
    "powershell",
    "pwsh",
];

/// How far up to look. Past a few shells the walk has left the agent.
const MAX_DEPTH: usize = 8;

/// The first non-shell ancestor, starting at `parent` — the hook's own.
pub fn find_agent(parent: u32, info: impl Fn(u32) -> Option<ProcInfo>) -> Option<AgentProcess> {
    let mut pid = parent;
    for _ in 0..MAX_DEPTH {
        if pid <= 1 {
            return None;
        }
        let proc = info(pid)?;
        if !SHELLS.contains(&proc.comm.as_str()) {
            return Some(AgentProcess {
                pid,
                start: proc.start,
            });
        }
        pid = proc.ppid;
    }
    None
}

/// Whether `agent` is still the process it was.
pub fn alive(agent: &AgentProcess, info: impl Fn(u32) -> Option<ProcInfo>) -> bool {
    info(agent.pid).is_some_and(|p| p.start == agent.start && !p.zombie)
}

/// `/proc/<pid>/stat`, parsed.
#[cfg(not(windows))]
pub fn read_proc(pid: u32) -> Option<ProcInfo> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// The process list's entry for `pid`, and when the process was created.
///
/// A process that has exited stays listed while anything holds a handle to
/// it, so its exit code is asked for too.
#[cfg(windows)]
pub fn read_proc(pid: u32) -> Option<ProcInfo> {
    use winsafe::{self as w, co};
    /// What `GetExitCodeProcess` reports for a process still running.
    const STILL_ACTIVE: u32 = 259;

    let mut list = w::HPROCESSLIST::CreateToolhelp32Snapshot(co::TH32CS::SNAPPROCESS, None).ok()?;
    let (ppid, exe) = list
        .iter_processes()
        .flatten()
        .find(|entry| entry.th32ProcessID == pid)
        .map(|entry| (entry.th32ParentProcessID, entry.szExeFile()))?;
    let process =
        w::HPROCESS::OpenProcess(co::PROCESS::QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let (created, ..) = process.GetProcessTimes().ok()?;
    let exe = exe.to_ascii_lowercase();
    Some(ProcInfo {
        ppid,
        comm: exe.strip_suffix(".exe").unwrap_or(&exe).to_string(),
        start: (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
        zombie: process.GetExitCodeProcess().ok()? != STILL_ACTIVE,
    })
}

/// The command name is in parentheses and may itself contain spaces and
/// parentheses, so the fields are counted from the *last* `)`.
#[cfg(any(not(windows), test))]
fn parse_stat(stat: &str) -> Option<ProcInfo> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?.to_string();
    // Fields after the name, numbered from 3 (state) as `proc(5)` numbers them.
    let rest: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    let field = |n: usize| rest.get(n - 3).copied();
    Some(ProcInfo {
        zombie: field(3)? == "Z",
        ppid: field(4)?.parse().ok()?,
        start: field(22)?.parse().ok()?,
        comm,
    })
}

/// The agent behind this process, the hook.
pub fn current_agent() -> Option<AgentProcess> {
    find_agent(parent_id()?, read_proc)
}

#[cfg(unix)]
fn parent_id() -> Option<u32> {
    Some(std::os::unix::process::parent_id())
}

/// std has no way to ask for a parent on Windows; the process list does.
#[cfg(windows)]
fn parent_id() -> Option<u32> {
    read_proc(std::process::id()).map(|me| me.ppid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn table(rows: &[(u32, u32, &str, u64)]) -> impl Fn(u32) -> Option<ProcInfo> {
        let map: HashMap<u32, ProcInfo> = rows
            .iter()
            .map(|&(pid, ppid, comm, start)| {
                (
                    pid,
                    ProcInfo {
                        ppid,
                        comm: comm.into(),
                        start,
                        zombie: false,
                    },
                )
            })
            .collect();
        move |pid| map.get(&pid).cloned()
    }

    #[test]
    fn the_agent_is_the_first_ancestor_that_is_not_a_shell() {
        // Antigravity, captured: thirdeye ← sh -c ← agy. Claude's plugin adds
        // its bash script in between.
        let procs = table(&[(30, 20, "sh", 5), (20, 10, "bash", 4), (10, 1, "agy", 3)]);
        assert_eq!(
            find_agent(30, &procs),
            Some(AgentProcess { pid: 10, start: 3 })
        );
        let direct = table(&[(10, 1, "copilot", 3)]);
        assert_eq!(
            find_agent(10, direct),
            Some(AgentProcess { pid: 10, start: 3 })
        );
    }

    #[test]
    fn a_walk_that_reaches_init_finds_no_agent() {
        let procs = table(&[(30, 1, "sh", 5)]);
        assert_eq!(find_agent(30, procs), None);
    }

    #[test]
    fn a_reused_pid_is_not_the_agent_coming_back() {
        let agent = AgentProcess { pid: 10, start: 3 };
        assert!(alive(&agent, table(&[(10, 1, "agy", 3)])));
        assert!(
            !alive(&agent, table(&[(10, 1, "vim", 99)])),
            "a new process"
        );
        assert!(!alive(&agent, table(&[])), "no process");
    }

    #[test]
    fn a_zombie_is_gone() {
        let agent = AgentProcess { pid: 10, start: 3 };
        let zombie = |_| {
            Some(ProcInfo {
                ppid: 1,
                comm: "agy".into(),
                start: 3,
                zombie: true,
            })
        };
        assert!(!alive(&agent, zombie));
    }

    #[test]
    fn a_command_name_with_parentheses_parses() {
        let stat = "4242 (my (odd) agent) S 4000 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 3 0 987654 1000 200";
        let p = parse_stat(stat).unwrap();
        assert_eq!(p.comm, "my (odd) agent");
        assert_eq!(p.ppid, 4000);
        assert_eq!(p.start, 987654);
        assert!(!p.zombie);
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn this_process_reads_as_alive() {
        // The real system, once: the reader agrees with what it is given.
        let me = std::process::id();
        let found = read_proc(me).expect("our own stat");
        assert!(alive(
            &AgentProcess {
                pid: me,
                start: found.start
            },
            read_proc
        ));
    }
}
