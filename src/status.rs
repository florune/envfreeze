//! 环境状态：运行/冻结、进程列表和内存占用。

use anyhow::Result;
use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

use crate::config::Config;
use crate::exclude::ExcludeRules;
use crate::job::Job;
use crate::memory::Memory;
use crate::process::Process;
use crate::state;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvState {
    Stopped,
    Running,
    Frozen,
    /// 有冻结记录但 Job 已不存在（holder 意外退出），进程可能仍被挂起。
    Orphaned,
}

impl EnvState {
    pub fn label(self) -> &'static str {
        match self {
            EnvState::Stopped => "未运行",
            EnvState::Running => "运行中",
            EnvState::Frozen => "已冻结",
            EnvState::Orphaned => "残留冻结记录",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessStatus {
    pub pid: u32,
    pub name: String,
    pub memory: Memory,
    /// 命中的用户白名单规则。
    pub excluded_by: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnvStatus {
    pub name: String,
    pub state: EnvState,
    pub processes: Vec<ProcessStatus>,
    /// 冻结时隐藏的任务栏窗口的标题，按 Z 序从上到下。
    pub hidden_windows: Vec<String>,
}

impl EnvStatus {
    pub fn total(&self) -> Memory {
        self.processes.iter().map(|p| p.memory).sum()
    }
}

pub fn env_state(env: &str) -> Result<EnvState> {
    let running = Job::open(env)?.is_some();
    let frozen = state::read(env)?.is_some();
    Ok(match (running, frozen) {
        (true, true) => EnvState::Frozen,
        (true, false) => EnvState::Running,
        (false, true) => EnvState::Orphaned,
        (false, false) => EnvState::Stopped,
    })
}

/// 冻结时隐藏的任务栏窗口的标题，没有冻结记录时为空。
pub fn hidden_windows(env: &str) -> Result<Vec<String>> {
    let record = state::read(env)?.unwrap_or_default();
    Ok(record.windows.into_iter().filter(|w| w.in_taskbar).map(|w| w.title).collect())
}

pub fn env_status(config: &Config, env: &str) -> Result<EnvStatus> {
    let rules = ExcludeRules::for_env(config, env);
    let state = env_state(env)?;
    let mut processes = Vec::new();
    if let Some(job) = Job::open(env)? {
        for pid in job.pids()? {
            let Ok(process) = Process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else { continue };
            let name = process.image_name().unwrap_or_else(|| "<未知>".into());
            processes.push(ProcessStatus {
                pid,
                excluded_by: rules.matches(&name).map(str::to_owned),
                name,
                memory: process.memory().unwrap_or_default(),
            });
        }
    }
    processes.sort_by_key(|p| std::cmp::Reverse(p.memory.working_set));
    Ok(EnvStatus { name: env.to_owned(), state, processes, hidden_windows: hidden_windows(env)? })
}

pub fn all(config: &Config) -> Result<Vec<EnvStatus>> {
    config.environments.keys().map(|name| env_status(config, name)).collect()
}
