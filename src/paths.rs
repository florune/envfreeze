//! 配置、冻结记录和日志的存放位置。

use std::path::PathBuf;

use anyhow::{Context, Result};

pub const CONFIG_ENV_VAR: &str = "ENVFREEZE_CONFIG";

/// 默认配置文件：`ENVFREEZE_CONFIG`，否则 `%APPDATA%\envfreeze\config.toml`。
pub fn default_config() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(CONFIG_ENV_VAR) {
        return Ok(PathBuf::from(path));
    }
    let dir = dirs::config_dir().context("无法确定 %APPDATA% 目录")?;
    Ok(dir.join("envfreeze").join("config.toml"))
}

/// `%LOCALAPPDATA%\envfreeze`
fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir().context("无法确定 %LOCALAPPDATA% 目录")?;
    Ok(dir.join("envfreeze"))
}

pub fn state_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("state"))
}

pub fn log_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("logs"))
}

pub fn holder_log(env: &str) -> Result<PathBuf> {
    Ok(log_dir()?.join(format!("{env}.log")))
}
