//! 关闭环境：先像用户点 X 那样关闭窗口，让程序自己保存退出；超时后由用户决定是否强制结束。
//!
//! 环境里的进程全部退出后，holder 会自动退出并清理冻结记录，环境回到"未运行"。

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tracing::{debug, info, warn};
use windows::Win32::System::Threading::{PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess};

use crate::freeze::{self, Failure, ThawReport};
use crate::guard;
use crate::job::Job;
use crate::process::Process;
use crate::state;
use crate::status::{self, EnvState};
use crate::win::describe;
use crate::window;

/// 等待程序自行退出的默认时长。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(300);
/// 强制结束后等进程真正退出的时长。
const KILL_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct Leftover {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct StopReport {
    pub env: String,
    /// 环境处于冻结状态时，关闭前先做的恢复。
    pub thawed: Option<ThawReport>,
    /// 发出关闭请求的窗口数。
    pub closed_windows: usize,
    /// 强制结束的进程数。
    pub killed: usize,
    /// 最后仍未退出的进程（不含系统白名单进程）。
    pub remaining: Vec<Leftover>,
    pub failures: Vec<Failure>,
    pub elapsed: Duration,
}

impl StopReport {
    pub fn ok(&self) -> bool {
        self.remaining.is_empty() && self.failures.is_empty()
    }

    pub fn summary(&self) -> String {
        let mut s = if self.remaining.is_empty() {
            format!("{}：已关闭", self.env)
        } else {
            let names: Vec<&str> = self.remaining.iter().map(|l| l.name.as_str()).collect();
            format!("{}：还有 {} 个进程没有退出（{}）", self.env, self.remaining.len(), names.join("、"))
        };
        if self.killed > 0 {
            s += &format!("，强制结束 {} 个进程", self.killed);
        }
        if !self.failures.is_empty() {
            s += &format!("，失败 {} 个", self.failures.len());
        }
        s
    }
}

/// 关闭环境：冻结中先恢复，再关闭所有窗口，最多等 `timeout` 让程序自行退出。
/// `force` 为 true 时，超时后强制结束剩下的进程。
pub fn stop(env: &str, timeout: Duration, force: bool) -> Result<StopReport> {
    let thawed = match status::env_state(env)? {
        EnvState::Stopped => bail!("环境 `{env}` 未运行"),
        EnvState::Orphaned => {
            // Job 不在了就分不清哪些进程属于环境，不能替用户关窗口。
            freeze::thaw(env)?;
            bail!("找不到环境 `{env}` 的 Job（holder 可能已退出），已恢复其中的进程，请手动关闭它们的窗口");
        }
        // 挂起的进程处理不了关闭请求，窗口也是隐藏的。
        EnvState::Frozen => Some(freeze::thaw(env)?),
        EnvState::Running => None,
    };
    let Some(job) = Job::open(env)? else { bail!("环境 `{env}` 未运行") };

    let started = Instant::now();
    let mut closed = HashSet::new();
    let mut remaining = owned_pids(&job)?;
    while !remaining.is_empty() && started.elapsed() < timeout {
        // 每轮都重新找窗口：刚恢复的环境窗口要过一会儿才显示出来。
        // 已经关过的窗口不再重复关闭，用户在"确认退出"之类的对话框里点了取消，就尊重这个选择。
        for w in window::visible(&remaining) {
            if w.in_taskbar && closed.insert(w.hwnd) {
                debug!(pid = w.pid, title = w.title, "关闭窗口");
                window::close(&w);
            }
        }
        std::thread::sleep(POLL);
        remaining = owned_pids(&job)?;
    }

    let mut report = StopReport {
        env: env.to_owned(),
        thawed,
        closed_windows: closed.len(),
        killed: 0,
        remaining: Vec::new(),
        failures: Vec::new(),
        elapsed: started.elapsed(),
    };
    if force && !remaining.is_empty() {
        terminate(&remaining, &mut report);
        remaining = wait_exit(&job, KILL_WAIT)?;
    }
    report.remaining = leftovers(&remaining);
    info!(env, "{}", report.summary());
    Ok(report)
}

/// 立即强制结束环境里的所有进程，冻结中的也一样（不需要先恢复）。未保存的内容会丢失。
pub fn kill(env: &str) -> Result<StopReport> {
    let Some(job) = Job::open(env)? else { bail!("环境 `{env}` 未运行") };
    let started = Instant::now();
    let mut report = StopReport {
        env: env.to_owned(),
        thawed: None,
        closed_windows: 0,
        killed: 0,
        remaining: Vec::new(),
        failures: Vec::new(),
        elapsed: Duration::ZERO,
    };
    terminate(&owned_pids(&job)?, &mut report);
    report.remaining = leftovers(&wait_exit(&job, KILL_WAIT)?);
    report.elapsed = started.elapsed();
    // holder 也会在环境清空后清理，这里先清，免得残留"已冻结"状态。
    if report.remaining.is_empty()
        && let Err(e) = state::clear(env)
    {
        warn!("{e:#}");
    }
    info!(env, "{}", report.summary());
    Ok(report)
}

/// Job 里除系统白名单进程以外的进程。系统进程（例如被带进环境的 explorer.exe）既不关窗口也不结束。
fn owned_pids(job: &Job) -> Result<HashSet<u32>> {
    Ok(guard::unprotected(&job.pids()?))
}

/// 强制结束之后，进程还要一小会儿才会真正退出、离开 Job。返回超时后仍在的进程。
fn wait_exit(job: &Job, timeout: Duration) -> Result<HashSet<u32>> {
    let started = Instant::now();
    loop {
        let remaining = owned_pids(job)?;
        if remaining.is_empty() || started.elapsed() >= timeout {
            return Ok(remaining);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn terminate(pids: &HashSet<u32>, report: &mut StopReport) {
    for &pid in pids {
        let process = match Process::open(pid, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION) {
            Ok(p) => p,
            // 已经退出了。
            Err(_) if Process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION).is_err() => continue,
            Err(e) => {
                report.failures.push(Failure { pid, name: "<无法打开>".into(), error: describe(&e) });
                continue;
            }
        };
        let name = process.image_name().unwrap_or_else(|| "<未知>".into());
        match unsafe { TerminateProcess(process.handle(), 1) } {
            Ok(()) => {
                debug!(pid, name, "已强制结束");
                report.killed += 1;
            }
            Err(e) => report.failures.push(Failure { pid, name, error: describe(&e) }),
        }
    }
}

fn leftovers(pids: &HashSet<u32>) -> Vec<Leftover> {
    let mut list: Vec<Leftover> = pids
        .iter()
        .map(|&pid| {
            let name = Process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)
                .ok()
                .and_then(|p| p.image_name())
                .unwrap_or_else(|| "<未知>".into());
            Leftover { pid, name }
        })
        .collect();
    list.sort_by_key(|l| l.pid);
    list
}
