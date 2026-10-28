//! 用户白名单：冻结时跳过的进程。
//!
//! 规则按进程映像名匹配，不区分大小写，支持 `*` 和 `?` 通配符。
//! 不带扩展名的规则（如 `navicat`）也会匹配 `navicat.exe`。
//! 生效规则 = 全局 `exclude` + 环境自己的 `exclude`。

use crate::config::Config;

#[derive(Debug, Clone, Default)]
pub struct ExcludeRules {
    patterns: Vec<String>,
}

impl ExcludeRules {
    pub fn new<I: IntoIterator<Item = S>, S: AsRef<str>>(patterns: I) -> Self {
        let patterns =
            patterns.into_iter().map(|p| p.as_ref().trim().to_lowercase()).filter(|p| !p.is_empty()).collect();
        Self { patterns }
    }

    pub fn for_env(config: &Config, env: &str) -> Self {
        let own = config.environments.get(env).map(|e| e.exclude.as_slice()).unwrap_or_default();
        Self::new(config.exclude.iter().chain(own))
    }

    /// 命中时返回匹配到的规则。
    pub fn matches(&self, image_name: &str) -> Option<&str> {
        let name = image_name.to_lowercase();
        let stem = name.strip_suffix(".exe").unwrap_or(&name);
        self.patterns.iter().find(|p| glob(p, &name) || (!p.contains('.') && glob(p, stem))).map(String::as_str)
    }
}

/// 简单通配符匹配：`*` 匹配任意串，`?` 匹配单个字符。
fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // 最近一个 `*` 的位置，以及它当时对应的文本位置，用于回溯。
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("navicat*.exe", "navicat premium.exe"));
        assert!(glob("*", "x"));
        assert!(glob("a?c", "abc"));
        assert!(!glob("a?c", "abbc"));
        assert!(glob("*code*", "claude-code.exe"));
        assert!(!glob("pwsh.exe", "pwsh.exe.bak"));
    }

    #[test]
    fn matches_case_insensitively_with_or_without_extension() {
        let rules = ExcludeRules::new(["Navicat", "PWSH.EXE", "  ", "node*"]);
        assert_eq!(rules.matches("navicat.exe"), Some("navicat"));
        assert_eq!(rules.matches("pwsh.exe"), Some("pwsh.exe"));
        assert_eq!(rules.matches("node.exe"), Some("node*"));
        assert_eq!(rules.matches("goland64.exe"), None);
    }

    #[test]
    fn combines_global_and_env_rules() {
        let config: Config = toml::from_str(
            r#"
            exclude = ["a.exe"]
            [environments.x]
            exclude = ["b.exe"]
            [environments.y]
            "#,
        )
        .unwrap();
        let x = ExcludeRules::for_env(&config, "x");
        assert!(x.matches("a.exe").is_some() && x.matches("b.exe").is_some());
        let y = ExcludeRules::for_env(&config, "y");
        assert!(y.matches("a.exe").is_some() && y.matches("b.exe").is_none());
    }
}
