//! JetBrains IDE（GoLand、IDEA、WebStorm……）的"每个环境一个独立实例"。
//!
//! JetBrains IDE 是单实例程序：同一份配置/系统目录只能有一个进程，再次启动只会把项目交给它。
//! 给每个环境生成一份 idea.properties，指定独立的 config/system 目录，
//! 再通过 `<产品>_PROPERTIES` 环境变量让该环境启动的 IDE 使用它，每个环境就有了自己的 IDE 进程。
//!
//! 插件目录指向原安装共用（插件动辄几百 MB，没必要每个环境复制一份），
//! 其余设置（主题、快捷键、许可证等，通常只有几十 MB）在首次创建时从现有配置复制。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JetBrainsIde {
    /// 产品名，例如 `GoLand`。
    pub name: String,
    pub install_dir: PathBuf,
    /// 例如 `GOLAND_PROPERTIES`。
    pub properties_env_var: String,
    /// 例如 `GoLand2026.1`，也是 `%APPDATA%\JetBrains` 下配置目录的名字。
    pub data_dir_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductInfo {
    name: String,
    env_var_base_name: String,
    data_directory_name: String,
}

/// 如果 `exe` 是 JetBrains IDE 的启动器（`<安装目录>\bin\xxx64.exe`），读出产品信息。
pub fn detect(exe: &Path) -> Option<JetBrainsIde> {
    let bin = exe.parent()?;
    if !bin.file_name()?.eq_ignore_ascii_case("bin") {
        return None;
    }
    let install_dir = bin.parent()?;
    let text = fs::read_to_string(install_dir.join("product-info.json")).ok()?;
    let info: ProductInfo = serde_json::from_str(&text).ok()?;
    Some(JetBrainsIde {
        name: info.name,
        install_dir: install_dir.to_owned(),
        properties_env_var: format!("{}_PROPERTIES", info.env_var_base_name),
        data_dir_name: info.data_directory_name,
    })
}

#[derive(Debug)]
pub struct Isolation {
    /// 生成的 idea.properties，把它设为 `properties_env_var` 的值。
    pub properties: PathBuf,
    /// 本次是否从现有配置复制了设置。
    pub copied_settings: bool,
    /// 复制时跳过的文件（通常是 IDE 正在运行时被锁住的文件）。
    pub skipped: Vec<String>,
}

/// 为环境 `env` 准备独立实例，位置在 `%APPDATA%\envfreeze\jetbrains\<env>\<数据目录名>`。
pub fn isolate(ide: &JetBrainsIde, env: &str, copy_settings: bool) -> Result<Isolation> {
    let appdata = dirs::config_dir().context("无法确定 %APPDATA% 目录")?;
    let base = appdata.join("envfreeze").join("jetbrains").join(env).join(&ide.data_dir_name);
    let shared = appdata.join("JetBrains").join(&ide.data_dir_name);
    isolate_in(ide, &base, &shared, copy_settings)
}

fn isolate_in(ide: &JetBrainsIde, base: &Path, shared: &Path, copy_settings: bool) -> Result<Isolation> {
    let config_dir = base.join("config");
    let system_dir = base.join("system");
    let mut skipped = Vec::new();
    // 只在首次创建时复制，之后该环境的设置独立演化，不能被覆盖。
    let copied_settings = copy_settings && !config_dir.exists() && shared.is_dir();
    if copied_settings {
        copy_dir(shared, &config_dir, &mut skipped)?;
    }
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&system_dir)?;

    // 在安装目录自带的 idea.properties 基础上追加覆盖项，保证其余默认值不丢。
    let original = fs::read_to_string(ide.install_dir.join("bin").join("idea.properties")).unwrap_or_default();
    let slash = |p: &Path| p.display().to_string().replace('\\', "/");
    let properties = format!(
        "{original}\n\n# ---- 由 envfreeze 生成：本环境独立的 {name} 实例 ----\n\
         idea.config.path={config}\n\
         idea.system.path={system}\n\
         idea.plugins.path={plugins}\n\
         idea.log.path={log}\n",
        name = ide.name,
        config = slash(&config_dir),
        system = slash(&system_dir),
        plugins = slash(&shared.join("plugins")),
        log = slash(&system_dir.join("log")),
    );
    let path = base.join("idea.properties");
    fs::write(&path, properties).with_context(|| format!("写入 {} 失败", path.display()))?;
    Ok(Isolation { properties: path, copied_settings, skipped })
}

/// 递归复制目录，跳过插件目录和锁文件；单个文件复制失败只记录不中断。
fn copy_dir(from: &Path, to: &Path, skipped: &mut Vec<String>) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from).with_context(|| format!("读取 {} 失败", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.eq_ignore_ascii_case("plugins") || name_str.ends_with(".lock") {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target, skipped)?;
        } else if let Err(e) = fs::copy(entry.path(), &target) {
            skipped.push(format!("{}：{e}", entry.path().display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("envfreeze-jb-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_install(root: &Path) -> PathBuf {
        let install = root.join("GoLand 2026.1");
        fs::create_dir_all(install.join("bin")).unwrap();
        fs::write(
            install.join("product-info.json"),
            r#"{"name":"GoLand","version":"2026.1","envVarBaseName":"GOLAND","dataDirectoryName":"GoLand2026.1"}"#,
        )
        .unwrap();
        fs::write(install.join("bin").join("idea.properties"), "idea.max.intellisense.filesize=2500\n").unwrap();
        install.join("bin").join("goland64.exe")
    }

    #[test]
    fn detects_ide_from_launcher_path() {
        let root = temp_dir("detect");
        let exe = fake_install(&root);
        let ide = detect(&exe).unwrap();
        assert_eq!(ide.name, "GoLand");
        assert_eq!(ide.properties_env_var, "GOLAND_PROPERTIES");
        assert_eq!(ide.data_dir_name, "GoLand2026.1");
        assert!(detect(&root.join("other").join("x.exe")).is_none());
    }

    #[test]
    fn isolates_with_shared_plugins_and_copied_settings() {
        let root = temp_dir("isolate");
        let ide = detect(&fake_install(&root)).unwrap();
        let shared = root.join("JetBrains").join("GoLand2026.1");
        fs::create_dir_all(shared.join("options")).unwrap();
        fs::create_dir_all(shared.join("plugins").join("big")).unwrap();
        fs::write(shared.join("options").join("ui.xml"), "<x/>").unwrap();
        fs::write(shared.join("goland.key"), "key").unwrap();
        fs::write(shared.join(".lock"), "1").unwrap();

        let base = root.join("envfreeze").join("proj-a").join("GoLand2026.1");
        let result = isolate_in(&ide, &base, &shared, true).unwrap();
        assert!(result.copied_settings);
        assert!(base.join("config").join("options").join("ui.xml").exists());
        assert!(base.join("config").join("goland.key").exists());
        assert!(!base.join("config").join("plugins").exists());
        assert!(!base.join("config").join(".lock").exists());

        let props = fs::read_to_string(&result.properties).unwrap();
        assert!(props.starts_with("idea.max.intellisense.filesize=2500"));
        assert!(props.contains("idea.plugins.path=") && props.contains("JetBrains/GoLand2026.1/plugins"));
        assert!(!props.contains('\\'), "properties 里不能有反斜杠：{props}");

        // 再次执行不会覆盖该环境已有的设置。
        fs::write(base.join("config").join("goland.key"), "changed").unwrap();
        let again = isolate_in(&ide, &base, &shared, true).unwrap();
        assert!(!again.copied_settings);
        assert_eq!(fs::read_to_string(base.join("config").join("goland.key")).unwrap(), "changed");
    }
}
