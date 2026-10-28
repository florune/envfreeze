//! `envfreeze launch`：启动 holder 后台进程，转述它回报的启动结果后立即返回。

use std::io::{BufRead, BufReader};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use tracing::warn;
use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
use windows::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

use crate::config::Config;
use crate::holder;
use crate::job::Job;
use crate::{paths, state};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LaunchReport {
    pub env: String,
    pub lines: Vec<(Level, String)>,
}

impl LaunchReport {
    pub fn ok(&self) -> bool {
        !self.lines.iter().any(|(level, _)| *level == Level::Error)
    }

    pub fn summary(&self) -> String {
        let count = |l| self.lines.iter().filter(|(level, _)| *level == l).count();
        let mut s = format!("{}：启动 {} 个程序", self.env, count(Level::Ok));
        if count(Level::Error) > 0 {
            s += &format!("，失败 {} 个", count(Level::Error));
        }
        if count(Level::Warn) > 0 {
            s += &format!("，警告 {} 条", count(Level::Warn));
        }
        s
    }
}

pub fn launch(config_path: &Path, env: &str) -> Result<LaunchReport> {
    // 先在前台校验配置，错误能直接显示给用户。
    Config::load(config_path)?.env(env)?;
    if Job::open(env)?.is_some() {
        bail!("环境 `{env}` 已在运行");
    }
    if state::read(env)?.is_some() {
        bail!("环境 `{env}` 有残留的冻结记录（之前的进程可能仍被挂起），请先执行 thaw");
    }

    let mut child = spawn_holder(config_path, env)?;
    let stdout = child.stdout.take().expect("stdout 已设置为管道");

    let mut report = LaunchReport { env: env.to_owned(), lines: Vec::new() };
    let mut finished = false;
    for line in BufReader::new(stdout).lines() {
        let line = line.context("读取 holder 输出失败")?;
        let (kind, msg) = line.split_once('\t').unwrap_or((line.as_str(), ""));
        let level = match kind {
            holder::DONE => {
                finished = true;
                break;
            }
            "OK" => Level::Ok,
            "WARN" => Level::Warn,
            _ => Level::Error,
        };
        report.lines.push((level, msg.to_owned()));
    }
    if !finished {
        bail!("holder 意外退出，详见日志 {}", paths::holder_log(env)?.display());
    }
    // 不等待 holder：它会一直运行到环境中的进程全部退出。
    drop(child);
    Ok(report)
}

fn spawn_holder(config_path: &Path, env: &str) -> Result<std::process::Child> {
    let exe = std::env::current_exe().context("无法获取 envfreeze 自身路径")?;
    // std 以 bInheritHandles=TRUE 创建子进程，会把所有可继承句柄都带给 holder。
    // 调用方给我们的 stdout 往往是管道（`envfreeze launch x | ...`、脚本捕获输出），
    // 如果 holder 继承了它，调用方会一直等不到 EOF，看起来就像 launch 卡住了。
    for handle in
        [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()]
    {
        let _ = unsafe { SetHandleInformation(HANDLE(handle), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
    }
    let config_path = std::path::absolute(config_path)?;
    let spawn = |flags: u32| {
        Command::new(&exe)
            .arg("__hold")
            .arg(env)
            .arg("--config")
            .arg(&config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };

    let base = (CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP).0;
    // 当前终端可能处在某个 Job 里（例如关闭终端会杀掉子进程的那种），
    // 先尝试让 holder 脱离它，这样环境的生命周期不依赖于这个终端。
    match spawn(base | CREATE_BREAKAWAY_FROM_JOB.0) {
        Ok(child) => Ok(child),
        Err(e) => {
            warn!("holder 无法脱离当前终端所在的 Job（{e}），关闭此终端可能会连带结束环境");
            spawn(base).context("启动 holder 失败")
        }
    }
}
