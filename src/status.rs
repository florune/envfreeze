//! `status`：各环境的运行/冻结状态和内存占用；指定环境名时列出进程明细。

use std::path::Path;

use anyhow::{Result, bail};
use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

use crate::config::Config;
use crate::job::Job;
use crate::memory::{Memory, fmt_bytes};
use crate::process::Process;
use crate::state;

struct Row {
    pid: u32,
    name: String,
    memory: Memory,
}

fn collect(job: &Job) -> Result<Vec<Row>> {
    Ok(job
        .pids()?
        .into_iter()
        .filter_map(|pid| {
            let process = Process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
            Some(Row {
                pid,
                name: process.image_name().unwrap_or_else(|| "<未知>".into()),
                memory: process.memory().unwrap_or_default(),
            })
        })
        .collect())
}

fn state_label(env: &str, running: bool) -> Result<&'static str> {
    let frozen = state::read(env)?.is_some();
    Ok(match (running, frozen) {
        (true, true) => "已冻结",
        (true, false) => "运行中",
        (false, true) => "未运行（残留冻结记录，可执行 thaw 恢复）",
        (false, false) => "未运行",
    })
}

pub fn run(config_path: &Path, env: Option<&str>) -> Result<()> {
    let config = Config::load(config_path)?;
    match env {
        Some(env) => detail(&config, env),
        None => overview(&config),
    }
}

fn overview(config: &Config) -> Result<()> {
    if config.environments.is_empty() {
        println!("配置中还没有环境");
        return Ok(());
    }
    println!("{:<20}  {:>5}  {:>11}  {:>11}  状态", "环境", "进程", "工作集", "私有提交");
    for name in config.environments.keys() {
        let job = Job::open(name)?;
        let rows = match &job {
            Some(job) => collect(job)?,
            None => Vec::new(),
        };
        let total: Memory = rows.iter().map(|r| r.memory).sum();
        println!(
            "{:<20}  {:>5}  {:>11}  {:>11}  {}",
            name,
            rows.len(),
            fmt_bytes(total.working_set),
            fmt_bytes(total.private),
            state_label(name, job.is_some())?
        );
    }
    Ok(())
}

fn detail(config: &Config, env: &str) -> Result<()> {
    config.env(env)?;
    let Some(job) = Job::open(env)? else {
        println!("环境 `{env}`：{}", state_label(env, false)?);
        return Ok(());
    };
    let mut rows = collect(&job)?;
    rows.sort_by_key(|r| std::cmp::Reverse(r.memory.working_set));
    println!("环境 `{env}`：{}，{} 个进程", state_label(env, true)?, rows.len());
    if rows.is_empty() {
        bail!("Job 存在但没有可访问的进程");
    }
    println!("{:>7}  {:>11}  {:>11}  进程", "PID", "工作集", "私有提交");
    for r in &rows {
        println!(
            "{:>7}  {:>11}  {:>11}  {}",
            r.pid,
            fmt_bytes(r.memory.working_set),
            fmt_bytes(r.memory.private),
            r.name
        );
    }
    Ok(())
}
