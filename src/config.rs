//! TOML 配置：每个环境列出要启动的程序。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::core::PCWSTR;

use crate::win::wide;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub environments: BTreeMap<String, Environment>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    #[serde(default)]
    pub programs: Vec<Program>,
}

/// 一个要启动的程序。path / args / cwd / env 的值都支持 `%VAR%` 展开。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub name: Option<String>,
    pub path: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// 额外环境变量，覆盖继承来的同名变量。
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl Program {
    pub fn display_name(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        Path::new(&self.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.clone())
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("读取配置文件 {} 失败", path.display()))?;
        let config: Config = toml::from_str(&text).with_context(|| format!("解析配置文件 {} 失败", path.display()))?;
        for name in config.environments.keys() {
            validate_env_name(name).with_context(|| format!("配置文件 {}", path.display()))?;
        }
        Ok(config)
    }

    pub fn env(&self, name: &str) -> Result<&Environment> {
        match self.environments.get(name) {
            Some(env) => Ok(env),
            None => {
                let known: Vec<&str> = self.environments.keys().map(String::as_str).collect();
                bail!("配置中没有环境 `{name}`，已有：{}", known.join(", "))
            }
        }
    }
}

/// 环境名会成为内核对象名的一部分，只允许安全字符。
pub fn validate_env_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!("环境名 `{name}` 不合法：只能包含字母、数字、- _ .，最长 64 个字符");
    }
    Ok(())
}

/// 展开 `%VAR%` 形式的环境变量；展开失败时原样返回。
pub fn expand(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let src = wide(s);
    let mut buf = vec![0u16; 1024];
    loop {
        let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf)) } as usize;
        if needed == 0 {
            return s.to_owned();
        }
        if needed <= buf.len() {
            return String::from_utf16_lossy(&buf[..needed - 1]);
        }
        buf.resize(needed, 0);
    }
}

pub fn expand_path(s: &str) -> PathBuf {
    PathBuf::from(expand(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let text = include_str!("../envfreeze.example.toml");
        let config: Config = toml::from_str(text).unwrap();
        assert!(!config.environments.is_empty());
    }

    #[test]
    fn rejects_bad_env_names() {
        assert!(validate_env_name("proj-a_1.x").is_ok());
        assert!(validate_env_name("").is_err());
        assert!(validate_env_name("a\\b").is_err());
        assert!(validate_env_name("项目").is_err());
    }

    #[test]
    fn expands_env_vars() {
        let windir = std::env::var("SystemRoot").unwrap();
        assert_eq!(expand(r"%SystemRoot%\x"), format!(r"{windir}\x"));
        assert_eq!(expand("no vars"), "no vars");
    }
}
