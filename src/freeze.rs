//! `freeze` / `thaw`：挂起或恢复环境 Job 中的全部进程。
//!
//! 单个进程失败不会中断流程：继续处理其余进程，最后在报告中汇总。

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tracing::{debug, warn};
use windows::Win32::Foundation::ERROR_INVALID_PARAMETER;
use windows::Win32::System::Threading::{
    PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_SUSPEND_RESUME,
};

use crate::exclude::ExcludeRules;
use crate::guard;
use crate::job::Job;
use crate::memory::{self, Memory, fmt_bytes};
use crate::process::Process;
use crate::state::{self, FrozenProcess};
use crate::win::{describe, is_win32};

const FREEZE_ACCESS: PROCESS_ACCESS_RIGHTS =
    PROCESS_ACCESS_RIGHTS(PROCESS_SUSPEND_RESUME.0 | PROCESS_QUERY_LIMITED_INFORMATION.0 | PROCESS_SET_QUOTA.0);
const THAW_ACCESS: PROCESS_ACCESS_RIGHTS =
    PROCESS_ACCESS_RIGHTS(PROCESS_SUSPEND_RESUME.0 | PROCESS_QUERY_LIMITED_INFORMATION.0);

/// 挂起过程中进程可能继续派生子进程，反复枚举直到没有新 PID，最多这么多轮。
const MAX_ROUNDS: usize = 10;

#[derive(Debug, Clone)]
pub struct Failure {
    pub pid: u32,
    pub name: String,
    pub error: String,
}

/// 没有冻结的进程及原因（系统白名单或用户白名单）。
#[derive(Debug, Clone)]
pub struct Skipped {
    pub pid: u32,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct FrozenRow {
    pub pid: u32,
    pub name: String,
    pub before: Memory,
    pub after: Memory,
}

#[derive(Debug, Clone)]
pub struct FreezeReport {
    pub env: String,
    /// 环境本来就处于冻结状态，本次什么都没做。
    pub already_frozen: bool,
    pub frozen: Vec<FrozenRow>,
    pub skipped: Vec<Skipped>,
    pub failures: Vec<Failure>,
    /// 多轮枚举后仍有新进程出现。
    pub unstable: bool,
    pub available_before: Option<u64>,
    pub available_after: Option<u64>,
    pub suspend_elapsed: Duration,
    pub trim_elapsed: Duration,
}

impl FreezeReport {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }

    /// 工作集释放量。
    pub fn released(&self) -> u64 {
        self.frozen.iter().map(|r| r.before.working_set.saturating_sub(r.after.working_set)).sum()
    }

    pub fn summary(&self) -> String {
        if self.already_frozen {
            return format!("{}：已处于冻结状态", self.env);
        }
        let mut s = format!("{}：冻结 {} 个进程，释放 {}", self.env, self.frozen.len(), fmt_bytes(self.released()));
        if !self.skipped.is_empty() {
            s += &format!("，跳过 {} 个", self.skipped.len());
        }
        if !self.failures.is_empty() {
            s += &format!("，失败 {} 个", self.failures.len());
        }
        s
    }
}

#[derive(Debug, Clone)]
pub struct ThawReport {
    pub env: String,
    pub resumed: usize,
    pub failures: Vec<Failure>,
    pub elapsed: Duration,
    /// 恢复前存在冻结记录。
    pub had_record: bool,
    /// 找不到 Job，只能按冻结记录恢复。
    pub job_missing: bool,
}

impl ThawReport {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn summary(&self) -> String {
        let mut s = format!("{}：恢复 {} 个进程", self.env, self.resumed);
        if !self.failures.is_empty() {
            s += &format!("，失败 {} 个（可重试）", self.failures.len());
        }
        s
    }
}

struct Suspended {
    process: Process,
    record: FrozenProcess,
    before: Memory,
}

enum Outcome<T> {
    Done(T),
    Skipped(Option<Skipped>),
    Failed(Failure),
}

pub fn freeze(env: &str, rules: &ExcludeRules) -> Result<FreezeReport> {
    let Some(job) = Job::open(env)? else { bail!("环境 `{env}` 未运行") };
    let mut report = FreezeReport {
        env: env.to_owned(),
        already_frozen: false,
        frozen: Vec::new(),
        skipped: Vec::new(),
        failures: Vec::new(),
        unstable: false,
        available_before: None,
        available_after: None,
        suspend_elapsed: Duration::ZERO,
        trim_elapsed: Duration::ZERO,
    };
    if state::read(env)?.is_some() {
        // 挂起是计数的，重复冻结会导致需要多次恢复。
        report.already_frozen = true;
        return Ok(report);
    }

    report.available_before = memory::available_physical();
    let started = Instant::now();
    let mut seen = HashSet::new();
    let mut suspended = Vec::new();
    report.unstable = true;
    for round in 0..MAX_ROUNDS {
        let new: Vec<u32> = job.pids()?.into_iter().filter(|pid| seen.insert(*pid)).collect();
        debug!(round, count = new.len(), "本轮新发现的进程");
        if new.is_empty() {
            report.unstable = false;
            break;
        }
        for pid in new {
            match suspend_one(pid, rules) {
                Outcome::Done(s) => suspended.push(s),
                Outcome::Skipped(s) => report.skipped.extend(s),
                Outcome::Failed(f) => report.failures.push(f),
            }
        }
    }
    if report.unstable {
        warn!("{MAX_ROUNDS} 轮后仍有新进程出现，可能有进程在不断派生子进程");
    }
    report.suspend_elapsed = started.elapsed();

    if suspended.is_empty() {
        // 全部被跳过或失败：没有挂起任何进程，不写冻结记录。
        return Ok(report);
    }
    // 先落盘再清空工作集：即使后续步骤出问题，thaw 也知道要恢复哪些进程。
    let records: Vec<FrozenProcess> = suspended.iter().map(|s| s.record.clone()).collect();
    if let Err(e) = state::write(env, &records) {
        warn!("{e:#}；thaw 仍可通过 Job 恢复进程，但无法防止重复冻结");
    }

    let trim_started = Instant::now();
    for s in suspended {
        if let Err(e) = s.process.empty_working_set() {
            report.failures.push(Failure {
                pid: s.record.pid,
                name: s.record.name.clone(),
                error: format!("清空工作集失败：{}", describe(&e)),
            });
        }
        let after = s.process.memory().unwrap_or_default();
        report.frozen.push(FrozenRow { pid: s.record.pid, name: s.record.name, before: s.before, after });
    }
    report.trim_elapsed = trim_started.elapsed();
    report.available_after = memory::available_physical();
    Ok(report)
}

fn suspend_one(pid: u32, rules: &ExcludeRules) -> Outcome<Suspended> {
    let process = match open(pid, FREEZE_ACCESS) {
        Outcome::Done(p) => p,
        Outcome::Skipped(s) => return Outcome::Skipped(s),
        Outcome::Failed(f) => return Outcome::Failed(f),
    };
    let name = process.image_name();
    if let Some(reason) = guard::protected_reason(pid, name.as_deref()) {
        warn!(pid, name = name.as_deref().unwrap_or("?"), "{reason}，不冻结");
        let name = name.unwrap_or_default();
        return Outcome::Skipped(Some(Skipped { pid, name, reason: reason.into() }));
    }
    let name = name.unwrap_or_else(|| "<未知>".into());
    if let Some(pattern) = rules.matches(&name) {
        debug!(pid, name, pattern, "命中用户白名单，不冻结");
        return Outcome::Skipped(Some(Skipped { pid, name, reason: format!("白名单 `{pattern}`") }));
    }
    let before = process.memory().unwrap_or_default();
    if let Err(e) = crate::ntdll::suspend(process.handle()) {
        return Outcome::Failed(Failure { pid, name, error: format!("{e:#}") });
    }
    debug!(pid, name, "已挂起");
    let record = FrozenProcess { pid, created: process.creation_time().unwrap_or(0), name };
    Outcome::Done(Suspended { process, record, before })
}

pub fn thaw(env: &str) -> Result<ThawReport> {
    let records = state::read(env)?;
    let job = Job::open(env)?;

    // (PID, 期望的创建时间)。来自 Job 的进程不需要校验创建时间。
    let mut targets: Vec<(u32, Option<u64>)> = Vec::new();
    match (&job, &records) {
        (Some(job), _) => {
            let pids = job.pids()?;
            targets.extend(pids.iter().map(|&pid| (pid, None)));
            // 冻结记录里有、但已不在 Job 里的进程（例如显式脱离了 Job）也要恢复。
            if let Some(records) = &records {
                targets.extend(records.iter().filter(|r| !pids.contains(&r.pid)).map(|r| (r.pid, Some(r.created))));
            }
        }
        (None, Some(records)) => {
            warn!("找不到环境 `{env}` 的 Job（holder 可能已退出），按冻结记录恢复");
            targets.extend(records.iter().map(|r| (r.pid, Some(r.created))));
        }
        (None, None) => bail!("环境 `{env}` 未运行"),
    }

    let started = Instant::now();
    let mut resumed = 0;
    let mut failures = Vec::new();
    for (pid, expected_created) in targets {
        match resume_one(pid, expected_created) {
            Outcome::Done(()) => resumed += 1,
            Outcome::Skipped(_) => {}
            Outcome::Failed(f) => failures.push(f),
        }
    }
    let report = ThawReport {
        env: env.to_owned(),
        resumed,
        failures,
        elapsed: started.elapsed(),
        had_record: records.is_some(),
        job_missing: job.is_none(),
    };
    // 有失败时保留记录：恢复一个未挂起的进程没有副作用，可以放心重试。
    if report.ok() {
        state::clear(env)?;
    }
    Ok(report)
}

fn resume_one(pid: u32, expected_created: Option<u64>) -> Outcome<()> {
    let process = match open(pid, THAW_ACCESS) {
        Outcome::Done(p) => p,
        Outcome::Skipped(s) => return Outcome::Skipped(s),
        Outcome::Failed(f) => return Outcome::Failed(f),
    };
    if let Some(expected) = expected_created.filter(|&c| c != 0)
        && process.creation_time() != Some(expected)
    {
        debug!(pid, "PID 已被其他进程复用，跳过");
        return Outcome::Skipped(None);
    }
    let name = process.image_name();
    if guard::protected_reason(pid, name.as_deref()).is_some() {
        return Outcome::Skipped(None);
    }
    let name = name.unwrap_or_else(|| "<未知>".into());
    match crate::ntdll::resume(process.handle()) {
        Ok(()) => {
            debug!(pid, name, "已恢复");
            Outcome::Done(())
        }
        Err(e) => Outcome::Failed(Failure { pid, name, error: format!("{e:#}") }),
    }
}

fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Outcome<Process> {
    match Process::open(pid, access) {
        Ok(p) => Outcome::Done(p),
        // 枚举之后进程已经退出。
        Err(e) if is_win32(&e, ERROR_INVALID_PARAMETER) => {
            debug!(pid, "进程已退出");
            Outcome::Skipped(None)
        }
        Err(e) => Outcome::Failed(Failure { pid, name: "<无法打开>".into(), error: describe(&e) }),
    }
}
