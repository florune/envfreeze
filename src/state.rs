//! 冻结记录：`%LOCALAPPDATA%\envfreeze\state\<环境名>.frozen`。
//!
//! 记录存在即表示环境处于冻结状态。它的作用是：
//! - 防止重复冻结（挂起是计数的，冻结两次就要恢复两次）；
//! - holder 意外退出、Job 名字消失时，thaw 仍能按记录的 PID + 创建时间恢复进程；
//! - 记下冻结时隐藏的窗口和原前台窗口，恢复时原样显示。
//!
//! 文件每行一条，字段用制表符分隔：进程 `pid created name`，
//! 窗口 `w hwnd pid in_taskbar(0/1) title`（按 Z 序从上到下），前台窗口 `f hwnd`。
//! 旧版本只有进程行，照样能读。

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

/// 冻结时被隐藏的顶层窗口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenWindow {
    pub hwnd: isize,
    pub pid: u32,
    /// 会出现在任务栏和 Alt+Tab 里的窗口，也就是用户认得出、需要展示给用户的那些。
    pub in_taskbar: bool,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    pub processes: Vec<FrozenProcess>,
    pub windows: Vec<HiddenWindow>,
    /// 冻结前的前台窗口（属于本环境时）。
    pub foreground: Option<isize>,
}

fn marker(env: &str) -> Result<PathBuf> {
    Ok(paths::state_dir()?.join(format!("{env}.frozen")))
}

pub fn read(env: &str) -> Result<Option<Record>> {
    let path = marker(env)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("读取冻结记录 {} 失败", path.display())),
    };
    Ok(Some(parse(&text)))
}

pub fn write(env: &str, record: &Record) -> Result<()> {
    let path = marker(env)?;
    fs::create_dir_all(path.parent().expect("冻结记录必有父目录"))?;
    fs::write(&path, render(record)).with_context(|| format!("写入冻结记录 {} 失败", path.display()))
}

pub fn clear(env: &str) -> Result<()> {
    let path = marker(env)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("删除冻结记录 {} 失败", path.display())),
    }
}

fn render(record: &Record) -> String {
    let mut out = String::new();
    for p in &record.processes {
        out += &format!("{}\t{}\t{}\n", p.pid, p.created, p.name);
    }
    for w in &record.windows {
        // 标题里的制表符和换行会破坏格式。
        let title: String = w.title.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        out += &format!("w\t{}\t{}\t{}\t{title}\n", w.hwnd, w.pid, u8::from(w.in_taskbar));
    }
    if let Some(f) = record.foreground {
        out += &format!("f\t{f}\n");
    }
    out
}

fn parse(text: &str) -> Record {
    let mut record = Record::default();
    for line in text.lines() {
        let mut fields = line.splitn(5, '\t');
        let Some(first) = fields.next() else { continue };
        let mut next = || fields.next();
        match first {
            "w" => {
                let window = (|| {
                    let hwnd = next()?.parse().ok()?;
                    let pid = next()?.parse().ok()?;
                    let in_taskbar = next()? == "1";
                    Some(HiddenWindow { hwnd, pid, in_taskbar, title: next().unwrap_or_default().to_owned() })
                })();
                record.windows.extend(window);
            }
            "f" => record.foreground = next().and_then(|f| f.parse().ok()),
            pid => {
                let process = (|| {
                    let pid = pid.parse().ok()?;
                    let created = next()?.parse().ok()?;
                    Some(FrozenProcess { pid, created, name: next().unwrap_or_default().to_owned() })
                })();
                record.processes.extend(process);
            }
        }
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let record = Record {
            processes: vec![
                FrozenProcess { pid: 42, created: 133_000_000_000_000_000, name: "goland64.exe".into() },
                FrozenProcess { pid: 7, created: 0, name: "Navicat Premium.exe".into() },
            ],
            windows: vec![
                HiddenWindow { hwnd: 0x1_0a2c, pid: 42, in_taskbar: true, title: "order-service – main.go".into() },
                HiddenWindow { hwnd: 77, pid: 7, in_taskbar: false, title: String::new() },
            ],
            foreground: Some(0x1_0a2c),
        };
        assert_eq!(parse(&render(&record)), record);
    }

    #[test]
    fn sanitizes_titles() {
        let record = Record {
            windows: vec![HiddenWindow { hwnd: 1, pid: 2, in_taskbar: true, title: "a\tb\nc".into() }],
            ..Default::default()
        };
        assert_eq!(parse(&render(&record)).windows[0].title, "a b c");
    }

    #[test]
    fn reads_old_format_and_skips_garbage() {
        let record = parse("oops\n1\t2\tx.exe\nw\tbad\n\n");
        assert_eq!(record.processes.len(), 1);
        assert!(record.windows.is_empty());
        assert_eq!(record.foreground, None);
    }
}
