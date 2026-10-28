//! 面板修改配置：用 toml_edit 只改动目标键，保留用户的注释和排版。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value, value};

use crate::config::{Config, validate_env_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalHotkey {
    Panel,
    FreezeAll,
}

impl GlobalHotkey {
    fn key(self) -> &'static str {
        match self {
            GlobalHotkey::Panel => "panel",
            GlobalHotkey::FreezeAll => "freeze_all",
        }
    }
}

pub fn set_env_hotkey(path: &Path, env: &str, hotkey: Option<&str>) -> Result<()> {
    edit(path, |doc| {
        let table = env_table(doc, env)?;
        set_or_remove(table, "hotkey", hotkey);
        Ok(())
    })
}

pub fn set_global_hotkey(path: &Path, which: GlobalHotkey, hotkey: Option<&str>) -> Result<()> {
    edit(path, |doc| {
        let table = sub_table(doc.as_table_mut(), "hotkeys")?;
        set_or_remove(table, which.key(), hotkey);
        Ok(())
    })
}

/// 设置白名单；`env` 为 None 时修改全局白名单。
pub fn set_exclude(path: &Path, env: Option<&str>, patterns: &[String]) -> Result<()> {
    edit(path, |doc| {
        let table = match env {
            Some(env) => env_table(doc, env)?,
            None => doc.as_table_mut(),
        };
        let patterns: Vec<&str> = patterns.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
        if patterns.is_empty() {
            table.remove("exclude");
        } else {
            table.insert("exclude", value(patterns.into_iter().collect::<Array>()));
        }
        Ok(())
    })
}

/// 要添加到环境的程序，字段含义同配置文件。
#[derive(Debug, Clone, Default)]
pub struct NewProgram {
    pub name: String,
    pub path: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
}

/// 新建一个空环境；同名环境已存在时报错。
pub fn add_env(path: &Path, env: &str) -> Result<()> {
    validate_env_name(env)?;
    edit(path, |doc| {
        let envs = sub_table(doc.as_table_mut(), "environments")?;
        if envs.contains_key(env) {
            bail!("环境 `{env}` 已存在");
        }
        envs.insert(env, Item::Table(Table::new()));
        Ok(())
    })
}

pub fn remove_env(path: &Path, env: &str) -> Result<()> {
    edit(path, |doc| {
        let envs = sub_table(doc.as_table_mut(), "environments")?;
        envs.remove(env).with_context(|| format!("环境 `{env}` 不存在"))?;
        Ok(())
    })
}

pub fn add_program(path: &Path, env: &str, program: &NewProgram) -> Result<()> {
    edit(path, |doc| {
        let table = env_table(doc, env)?;
        let programs = table
            .entry("programs")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .context("配置中的 `programs` 不是表数组")?;

        let mut entry = Table::new();
        if !program.name.trim().is_empty() {
            entry.insert("name", value(program.name.trim()));
        }
        entry.insert("path", Item::Value(str_value(&program.path)));
        let args: Vec<&String> = program.args.iter().filter(|a| !a.is_empty()).collect();
        if !args.is_empty() {
            entry.insert("args", value(args.into_iter().map(|a| str_value(a)).collect::<Array>()));
        }
        if let Some(cwd) = program.cwd.as_deref().filter(|c| !c.trim().is_empty()) {
            entry.insert("cwd", Item::Value(str_value(cwd.trim())));
        }
        if !program.env.is_empty() {
            let mut vars = InlineTable::new();
            for (k, v) in &program.env {
                vars.insert(k, str_value(v));
            }
            entry.insert("env", value(vars));
        }
        programs.push(entry);
        Ok(())
    })
}

/// 删除环境中第 `index` 个程序（与配置文件中的顺序一致）。
pub fn remove_program(path: &Path, env: &str, index: usize) -> Result<()> {
    edit(path, |doc| {
        let table = env_table(doc, env)?;
        let programs = table.get_mut("programs").and_then(Item::as_array_of_tables_mut).context("环境中没有程序")?;
        if index >= programs.len() {
            bail!("程序序号 {index} 超出范围");
        }
        programs.remove(index);
        if programs.is_empty() {
            table.remove("programs");
        }
        Ok(())
    })
}

/// Windows 路径里全是反斜杠，尽量写成 TOML 字面量字符串 `'D:\x'`，免得满屏 `\\`。
fn str_value(s: &str) -> Value {
    if !s.contains(['\'', '\n', '\r'])
        && let Ok(v) = format!("'{s}'").parse::<Value>()
    {
        return v;
    }
    Value::from(s)
}

/// 读取 → 修改 → 校验 → 原子写回。
fn edit(path: &Path, f: impl FnOnce(&mut DocumentMut) -> Result<()>) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("读取配置文件 {} 失败", path.display()))?;
    let mut doc: DocumentMut = text.parse().with_context(|| format!("解析配置文件 {} 失败", path.display()))?;
    f(&mut doc)?;
    let new = doc.to_string();
    // 写回前确认改完的配置仍然合法，避免把文件写坏。
    toml::from_str::<Config>(&new).context("修改后的配置无法解析")?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, &new).with_context(|| format!("写入 {} 失败", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("替换配置文件 {} 失败", path.display()))
}

fn env_table<'a>(doc: &'a mut DocumentMut, env: &str) -> Result<&'a mut Table> {
    let envs = sub_table(doc.as_table_mut(), "environments")?;
    sub_table(envs, env)
}

fn sub_table<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table> {
    let item = parent.entry(key).or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    item.as_table_mut().with_context(|| format!("配置中的 `{key}` 不是表"))
}

fn set_or_remove(table: &mut Table, key: &str, v: Option<&str>) {
    match v {
        Some(v) => {
            table.insert(key, value(v));
        }
        None => {
            table.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# 顶部注释
[[environments.proj-a.programs]]
path = 'a.exe'  # 行尾注释

[[environments.proj-b.programs]]
path = 'b.exe'
"#;

    fn with_file(f: impl FnOnce(&Path)) {
        let dir = std::env::temp_dir().join(format!("envfreeze-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{:?}.toml", std::thread::current().id()).replace(['(', ')'], ""));
        fs::write(&path, SAMPLE).unwrap();
        f(&path);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn edits_keep_comments_and_round_trip() {
        with_file(|path| {
            set_env_hotkey(path, "proj-a", Some("Ctrl+Alt+1")).unwrap();
            set_global_hotkey(path, GlobalHotkey::Panel, Some("Ctrl+Alt+Space")).unwrap();
            set_exclude(path, None, &["navicat.exe".into(), " ".into()]).unwrap();
            set_exclude(path, Some("proj-b"), &["node*".into()]).unwrap();

            let text = fs::read_to_string(path).unwrap();
            assert!(text.contains("# 顶部注释") && text.contains("# 行尾注释"), "{text}");
            let config: Config = toml::from_str(&text).unwrap();
            assert_eq!(config.environments["proj-a"].hotkey.as_deref(), Some("Ctrl+Alt+1"));
            assert_eq!(config.environments["proj-a"].programs.len(), 1);
            assert_eq!(config.hotkeys.panel.as_deref(), Some("Ctrl+Alt+Space"));
            assert_eq!(config.exclude, ["navicat.exe"]);
            assert_eq!(config.environments["proj-b"].exclude, ["node*"]);

            set_env_hotkey(path, "proj-a", None).unwrap();
            set_exclude(path, None, &[]).unwrap();
            let config: Config = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            assert!(config.environments["proj-a"].hotkey.is_none());
            assert!(config.exclude.is_empty());
        });
    }

    #[test]
    fn adds_and_removes_envs_and_programs() {
        with_file(|path| {
            add_env(path, "proj-c").unwrap();
            assert!(add_env(path, "proj-c").is_err());
            assert!(add_env(path, "bad name").is_err());

            let program = NewProgram {
                name: "GoLand".into(),
                path: r"D:\idea\GoLand 2026.1\bin\goland64.exe".into(),
                args: vec![r"D:\code\x".into(), r"it's".into()],
                cwd: Some(r"D:\code".into()),
                env: BTreeMap::from([("GOLAND_PROPERTIES".into(), r"C:\p\idea.properties".into())]),
            };
            add_program(path, "proj-c", &program).unwrap();
            add_program(path, "proj-c", &NewProgram { path: "b.exe".into(), ..Default::default() }).unwrap();

            let text = fs::read_to_string(path).unwrap();
            assert!(text.contains(r"path = 'D:\idea\GoLand 2026.1\bin\goland64.exe'"), "{text}");
            let config: Config = toml::from_str(&text).unwrap();
            let programs = &config.environments["proj-c"].programs;
            assert_eq!(programs.len(), 2);
            assert_eq!(programs[0].args, [r"D:\code\x", "it's"]);
            assert_eq!(programs[0].cwd.as_deref(), Some(r"D:\code"));
            assert_eq!(programs[0].env["GOLAND_PROPERTIES"], r"C:\p\idea.properties");

            remove_program(path, "proj-c", 0).unwrap();
            let config: Config = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(config.environments["proj-c"].programs[0].path, "b.exe");

            remove_env(path, "proj-c").unwrap();
            remove_env(path, "proj-a").unwrap();
            let config: Config = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(config.environments.keys().collect::<Vec<_>>(), ["proj-b"]);
        });
    }
}
