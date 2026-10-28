//! egui 自带字体没有中文，从系统字体目录加载一个中文字体。

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

const CANDIDATES: &[&str] =
    &[r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\simhei.ttf", r"C:\Windows\Fonts\simsun.ttc"];

pub fn install(ctx: &egui::Context) {
    let Some(bytes) = CANDIDATES.iter().find_map(|path| std::fs::read(path).ok()) else {
        tracing::warn!("没有找到中文字体，面板中的中文将无法显示");
        return;
    };
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("cjk".into(), Arc::new(FontData::from_owned(bytes)));
    // 正文优先用中文字体；等宽字体保留默认字体，中文作为后备。
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "cjk".into());
    fonts.families.entry(FontFamily::Monospace).or_default().push("cjk".into());
    ctx.set_fonts(fonts);
}
