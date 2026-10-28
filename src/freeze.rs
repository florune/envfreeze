//! `freeze` / `thaw`：挂起或恢复环境 Job 中的全部进程。
//!
//! 单个进程失败不会中断流程：继续处理其余进程，最后汇总报错。

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tracing::{debug, warn};
use windows::Win32::Foundation::ERROR_INVALID_PARAMETER;
use windows::Win32::System::Threading::{
    PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_SUSPEND_RESUME,
};

use crate::guard;
use crate::job::Job;
use crate::memory::{self, Memory, fmt_bytes, fmt_delta};
use crate::process::Process;
use crate::state::{self, FrozenProcess};
use crate::win::{describe, is_win32};

const FREEZE_ACCESS: PROCESS_ACCESS_RIGHTS =
    PROCESS_ACCESS_RIGHTS(PROCESS_SUSPEND_RESUME.0 | PROCESS_QUERY_LIMITED_INFORMATION.0 | PROCESS_SET_QUOTA.0);
const THAW_ACCESS: PROCESS_ACCESS_RIGHTS =
    PROCESS_ACCESS_RIGHTS(PROCESS_SUSPEND_RESUME.0 | PROCESS_QUERY_LIMITED_INFORMATION.0);

/// 挂起过程中进程可能继续派生子进程，反复枚举直到没有新 PID，最多这么多轮。
const MAX_ROUNDS: usize = 10;

struct Failure {
    pid: u32,
    name: String,
    error: String,
}

struct Suspended {
    process: Process,
    record: FrozenProcess,
    before: Memory,
}

enum Outcome<T> {
    Done(T),
    /// 进程已退出或在白名单中，不算失败。
    Skipped,
    Failed(Failure),
}

/// 返回是否全部成功。
pub fn freeze(env: &str) -> Result<bool> {
    let Some(job) = Job::open(env)? else { bail!("环境 `{env}` 未运行") };
    if state::read(env)?.is_some() {
        println!("环境 `{env}` 已处于冻结状态，跳过（重复挂起会导致需要多次恢复）");
        return Ok(true);
    }

    let available_before = memory::available_physical();
    let started = Instant::now();
    let mut seen = HashSet::new();
    let mut suspended = Vec::new();
    let mut failures = Vec::new();
    let mut stable = false;
    for round in 0..MAX_ROUNDS {
        let new: Vec<u32> = job.pids()?.into_iter().filter(|pid| seen.insert(*pid)).collect();
        debug!(round, count = new.len(), "本轮新发现的进程");
        if new.is_empty() {
            stable = true;
            break;
        }
        for pid in new {
            match suspend_one(pid) {
                Outcome::Done(s) => suspended.push(s),
                Outcome::Skipped => {}
                Outcome::Failed(f) => failures.push(f),
            }
        }
    }
    if !stable {
        warn!("{MAX_ROUNDS} 轮后仍有新进程出现，可能有进程在不断派生子进程");
    }
    let suspend_elapsed = started.elapsed();

    if suspended.is_empty() {
        print_failures(&failures);
        bail!("环境 `{env}` 中没有成功挂起的进程");
    }
    // 先落盘再清空工作集：即使后续步骤出问题，thaw 也知道要恢复哪些进程。
    let records: Vec<FrozenProcess> = suspended.iter().map(|s| s.record.clone()).collect();
    if let Err(e) = state::write(env, &records) {
        warn!("{e:#}；thaw 仍可通过 Job 恢复进程，但无法防止重复冻结");
    }

    let trim_started = Instant::now();
    let mut rows = Vec::new();
    for s in &suspended {
        if let Err(e) = s.process.empty_working_set() {
            failures.push(Failure {
                pid: s.record.pid,
                name: s.record.name.clone(),
                error: format!("清空工作集失败：{}", describe(&e)),
            });
        }
        rows.push((s, s.process.memory().unwrap_or_default()));
    }
    let trim_elapsed = trim_started.elapsed();
    let available_after = memory::available_physical();

    println!("已冻结环境 `{env}`：挂起 {} 个进程", suspended.len());
    println!("{:>7}  {:>11}  {:>11}  {:>11}  进程", "PID", "工作集(前)", "工作集(后)", "私有提交");
    for (s, after) in &rows {
        println!(
            "{:>7}  {:>11}  {:>11}  {:>11}  {}",
            s.record.pid,
            fmt_bytes(s.before.working_set),
            fmt_bytes(after.working_set),
            fmt_bytes(after.private),
            s.record.name
        );
    }
    let before: Memory = rows.iter().map(|(s, _)| s.before).sum();
    let after: Memory = rows.iter().map(|(_, m)| *m).sum();
    println!(
        "{:>7}  {:>11}  {:>11}  {:>11}  工作集变化 {}",
        "合计",
        fmt_bytes(before.working_set),
        fmt_bytes(after.working_set),
        fmt_bytes(after.private),
        fmt_delta(before.working_set, after.working_set)
    );
    if let (Some(b), Some(a)) = (available_before, available_after) {
        println!("系统可用物理内存：{} → {}（{}）", fmt_bytes(b), fmt_bytes(a), fmt_delta(b, a));
        println!("  注：被换出的页面会先进入修改列表/内存压缩，可用内存可能在之后几秒内继续上升");
    }
    println!("挂起用时 {}，清空工作集用时 {}", fmt_duration(suspend_elapsed), fmt_duration(trim_elapsed));
    print_failures(&failures);
    Ok(failures.is_empty())
}

fn suspend_one(pid: u32) -> Outcome<Suspended> {
    let process = match open(pid, FREEZE_ACCESS) {
        Outcome::Done(p) => p,
        Outcome::Skipped => return Outcome::Skipped,
        Outcome::Failed(f) => return Outcome::Failed(f),
    };
    let name = process.image_name();
    if let Some(reason) = guard::protected_reason(pid, name.as_deref()) {
        warn!(pid, name = name.as_deref().unwrap_or("?"), "{reason}，不冻结");
        return Outcome::Skipped;
    }
    let name = name.unwrap_or_else(|| "<未知>".into());
    let before = process.memory().unwrap_or_default();
    if let Err(e) = crate::ntdll::suspend(process.handle()) {
        return Outcome::Failed(Failure { pid, name, error: format!("{e:#}") });
    }
    debug!(pid, name, "已挂起");
    let record = FrozenProcess { pid, created: process.creation_time().unwrap_or(0), name };
    Outcome::Done(Suspended { process, record, before })
}

/// 返回是否全部成功。
pub fn thaw(env: &str) -> Result<bool> {
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
    if records.is_none() {
        println!("环境 `{env}` 没有冻结记录，仍对其中所有进程执行一次恢复");
    }

    let started = Instant::now();
    let mut resumed = 0;
    let mut failures = Vec::new();
    for (pid, expected_created) in targets {
        match resume_one(pid, expected_created) {
            Outcome::Done(()) => resumed += 1,
            Outcome::Skipped => {}
            Outcome::Failed(f) => failures.push(f),
        }
    }
    let elapsed = started.elapsed();

    println!("已恢复环境 `{env}`：{resumed} 个进程，用时 {}", fmt_duration(elapsed));
    println!("  注：内存会在程序被使用时按需换入，首次切回窗口可能有短暂卡顿");
    print_failures(&failures);
    if failures.is_empty() {
        state::clear(env)?;
    } else {
        // 保留记录：恢复一个未挂起的进程不会有副作用，可以放心重试。
        println!("冻结记录已保留，可再次执行 `envfreeze thaw {env}` 重试");
    }
    Ok(failures.is_empty())
}

fn resume_one(pid: u32, expected_created: Option<u64>) -> Outcome<()> {
    let process = match open(pid, THAW_ACCESS) {
        Outcome::Done(p) => p,
        Outcome::Skipped => return Outcome::Skipped,
        Outcome::Failed(f) => return Outcome::Failed(f),
    };
    if let Some(expected) = expected_created.filter(|&c| c != 0)
        && process.creation_time() != Some(expected)
    {
        debug!(pid, "PID 已被其他进程复用，跳过");
        return Outcome::Skipped;
    }
    let name = process.image_name();
    if guard::protected_reason(pid, name.as_deref()).is_some() {
        return Outcome::Skipped;
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
            Outcome::Skipped
        }
        Err(e) => Outcome::Failed(Failure { pid, name: "<无法打开>".into(), error: describe(&e) }),
    }
}

fn print_failures(failures: &[Failure]) {
    if failures.is_empty() {
        return;
    }
    eprintln!("失败 {} 个：", failures.len());
    for f in failures {
        eprintln!("  PID {} {}：{}", f.pid, f.name, f.error);
    }
}

fn fmt_duration(d: Duration) -> String {
    if d.as_secs() >= 1 { format!("{:.2} s", d.as_secs_f64()) } else { format!("{} ms", d.as_millis()) }
}
