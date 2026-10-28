//! 冻结记录：`%LOCALAPPDATA%\envfreeze\state\<环境名>.frozen`。
//!
//! 记录存在即表示环境处于冻结状态。它的作用是：
//! - 防止重复冻结（挂起是计数的，冻结两次就要恢复两次）；
//! - holder 意外退出、Job 名字消失时，thaw 仍能按记录的 PID + 创建时间恢复进程。

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenProcess {
    pub pid: u32,
    /// 进程创建时间，用于识别 PID 复用；0 表示未知。
    pub created: u64,
    pub name: String,
}

fn marker(env: &str) -> Result<PathBuf> {
    Ok(paths::state_dir()?.join(format!("{env}.frozen")))
}

pub fn read(env: &str) -> Result<Option<Vec<FrozenProcess>>> {
    let path = marker(env)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("读取冻结记录 {} 失败", path.display())),
    };
    Ok(Some(parse(&text)))
}

pub fn write(env: &str, processes: &[FrozenProcess]) -> Result<()> {
    let path = marker(env)?;
    fs::create_dir_all(path.parent().expect("冻结记录必有父目录"))?;
    fs::write(&path, render(processes)).with_context(|| format!("写入冻结记录 {} 失败", path.display()))
}

pub fn clear(env: &str) -> Result<()> {
    let path = marker(env)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("删除冻结记录 {} 失败", path.display())),
    }
}

fn render(processes: &[FrozenProcess]) -> String {
    processes.iter().map(|p| format!("{}\t{}\t{}\n", p.pid, p.created, p.name)).collect()
}

fn parse(text: &str) -> Vec<FrozenProcess> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let pid = fields.next()?.parse().ok()?;
            let created = fields.next()?.parse().ok()?;
            let name = fields.next().unwrap_or_default().to_owned();
            Some(FrozenProcess { pid, created, name })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let processes = vec![
            FrozenProcess { pid: 42, created: 133_000_000_000_000_000, name: "goland64.exe".into() },
            FrozenProcess { pid: 7, created: 0, name: "Navicat Premium.exe".into() },
        ];
        assert_eq!(parse(&render(&processes)), processes);
    }

    #[test]
    fn skips_garbage_lines() {
        assert_eq!(parse("oops\n1\t2\tx.exe\n\n").len(), 1);
    }
}
