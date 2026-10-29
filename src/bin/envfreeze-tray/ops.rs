//! 托盘快捷键和面板按钮触发的环境操作，执行后给出一段适合通知/日志展示的结果。

use std::path::Path;

use envfreeze::config::Config;
use envfreeze::launch::{LaunchReport, Level};
use envfreeze::switch::{self, Action, Batch};
use envfreeze::{freeze, launch, stop};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvOp {
    Switch(String),
    Launch(String),
    Freeze(String),
    Thaw(String),
    /// 关闭窗口，等程序自行退出。
    Stop(String),
    /// 强制结束全部进程。
    Kill(String),
    FreezeAll,
}

impl EnvOp {
    pub fn describe(&self) -> String {
        match self {
            EnvOp::Switch(env) => format!("切换到 {env}"),
            EnvOp::Launch(env) => format!("启动 {env}"),
            EnvOp::Freeze(env) => format!("冻结 {env}"),
            EnvOp::Thaw(env) => format!("恢复 {env}"),
            EnvOp::Stop(env) => format!("关闭 {env}"),
            EnvOp::Kill(env) => format!("强制结束 {env}"),
            EnvOp::FreezeAll => "冻结全部环境".into(),
        }
    }
}

pub struct OpResult {
    pub ok: bool,
    pub title: String,
    pub body: String,
}

pub fn execute(config_path: &Path, op: &EnvOp) -> OpResult {
    let title = op.describe();
    let result = match op {
        EnvOp::Switch(env) => switch::switch(config_path, env).map(|b| from_batch(&b)),
        EnvOp::FreezeAll => switch::freeze_all(config_path).map(|b| from_batch(&b)),
        EnvOp::Launch(env) => launch::launch(config_path, env).map(|r| (r.ok(), launch_text(&r))),
        EnvOp::Freeze(env) => {
            Config::load(config_path).and_then(|config| switch::freeze_env(&config, env)).map(|r| (r.ok(), r.summary()))
        }
        EnvOp::Thaw(env) => freeze::thaw(env).map(|r| (r.ok(), r.summary())),
        EnvOp::Stop(env) => stop::stop(env, stop::DEFAULT_TIMEOUT, false).map(|r| {
            let mut body = r.summary();
            if !r.remaining.is_empty() {
                body += "\n程序可能在等你确认；也可以在面板里强制结束";
            }
            (r.ok(), body)
        }),
        EnvOp::Kill(env) => stop::kill(env).map(|r| (r.ok(), r.summary())),
    };
    match result {
        Ok((ok, body)) => OpResult { ok, title, body },
        Err(e) => OpResult { ok: false, title, body: format!("{e:#}") },
    }
}

fn from_batch(batch: &Batch) -> (bool, String) {
    if batch.steps.is_empty() {
        return (true, "无需操作".into());
    }
    let lines: Vec<String> = batch
        .steps
        .iter()
        .map(|step| match &step.result {
            Ok(Action::Launched(r)) => launch_text(r),
            _ => step.summary(),
        })
        .collect();
    (batch.ok(), lines.join("\n"))
}

/// 启动摘要，加上所有警告和错误（单实例警告很重要，不能只给个计数）。
fn launch_text(report: &LaunchReport) -> String {
    let mut lines = vec![report.summary()];
    lines.extend(report.lines.iter().filter(|(level, _)| *level != Level::Ok).map(|(_, msg)| msg.clone()));
    lines.join("\n")
}
