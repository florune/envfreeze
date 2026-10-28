//! 组合操作：切换环境、冻结全部。供命令行、托盘快捷键和面板共用。

use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::exclude::ExcludeRules;
use crate::freeze::{self, FreezeReport, ThawReport};
use crate::launch::{self, LaunchReport};
use crate::status::{self, EnvState};

#[derive(Debug, Clone)]
pub enum Action {
    Froze(FreezeReport),
    Thawed(ThawReport),
    Launched(LaunchReport),
    AlreadyActive(String),
}

impl Action {
    pub fn ok(&self) -> bool {
        match self {
            Action::Froze(r) => r.ok(),
            Action::Thawed(r) => r.ok(),
            Action::Launched(r) => r.ok(),
            Action::AlreadyActive(_) => true,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Action::Froze(r) => r.summary(),
            Action::Thawed(r) => r.summary(),
            Action::Launched(r) => r.summary(),
            Action::AlreadyActive(env) => format!("{env}：已在运行"),
        }
    }
}

/// 一次组合操作中对某个环境做的一步。
#[derive(Debug, Clone)]
pub struct Step {
    pub env: String,
    pub result: Result<Action, String>,
}

impl Step {
    fn new(env: &str, result: Result<Action>) -> Self {
        Self { env: env.to_owned(), result: result.map_err(|e| format!("{e:#}")) }
    }

    pub fn ok(&self) -> bool {
        self.result.as_ref().is_ok_and(Action::ok)
    }

    pub fn summary(&self) -> String {
        match &self.result {
            Ok(action) => action.summary(),
            Err(e) => format!("{}：{e}", self.env),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Batch {
    pub title: String,
    pub steps: Vec<Step>,
}

impl Batch {
    pub fn ok(&self) -> bool {
        self.steps.iter().all(Step::ok)
    }

    pub fn summary(&self) -> String {
        if self.steps.is_empty() {
            return format!("{}：无需操作", self.title);
        }
        let lines: Vec<String> = self.steps.iter().map(Step::summary).collect();
        format!("{}\n{}", self.title, lines.join("\n"))
    }
}

pub fn freeze_env(config: &Config, env: &str) -> Result<FreezeReport> {
    config.env(env)?;
    freeze::freeze(env, &ExcludeRules::for_env(config, env))
}

/// 让目标环境处于运行状态：未运行则启动，冻结中则恢复。
pub fn activate(config_path: &Path, env: &str) -> Result<Action> {
    Ok(match status::env_state(env)? {
        EnvState::Stopped => Action::Launched(launch::launch(config_path, env)?),
        // 残留记录说明之前的进程可能还挂起着，先把它们恢复。
        EnvState::Frozen | EnvState::Orphaned => Action::Thawed(freeze::thaw(env)?),
        EnvState::Running => Action::AlreadyActive(env.to_owned()),
    })
}

/// 切换到目标环境：先冻结其他所有运行中的环境腾出内存，再激活目标环境。
pub fn switch(config_path: &Path, target: &str) -> Result<Batch> {
    let config = Config::load(config_path)?;
    config.env(target)?;
    let mut steps = Vec::new();
    for env in config.environments.keys().filter(|e| *e != target) {
        if status::env_state(env)? == EnvState::Running {
            steps.push(Step::new(env, freeze_env(&config, env).map(Action::Froze)));
        }
    }
    steps.push(Step::new(target, activate(config_path, target)));
    Ok(Batch { title: format!("切换到 {target}"), steps })
}

/// 冻结所有运行中的环境。
pub fn freeze_all(config_path: &Path) -> Result<Batch> {
    let config = Config::load(config_path)?;
    let mut steps = Vec::new();
    for env in config.environments.keys() {
        if status::env_state(env)? == EnvState::Running {
            steps.push(Step::new(env, freeze_env(&config, env).map(Action::Froze)));
        }
    }
    Ok(Batch { title: "冻结全部环境".into(), steps })
}
