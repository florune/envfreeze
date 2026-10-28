//! 托盘图标：按整体状态着色的圆点，中间是一个简化的雪花。

use envfreeze::status::EnvState;

const SIZE: u32 = 32;

/// 有环境被冻结 → 冰蓝；只有运行中的环境 → 绿；都没运行 → 灰。
pub fn color_for(states: &[EnvState]) -> [u8; 3] {
    if states.iter().any(|s| matches!(s, EnvState::Frozen | EnvState::Orphaned)) {
        [56, 152, 236]
    } else if states.contains(&EnvState::Running) {
        [46, 170, 92]
    } else {
        [128, 128, 128]
    }
}

pub fn render(color: [u8; 3]) -> tray_icon::Icon {
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let c = (SIZE as f32 - 1.0) / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt();
            // 圆的边缘做一个像素的抗锯齿。
            let alpha = (c + 0.5 - r).clamp(0.0, 1.0);
            if alpha <= 0.0 {
                continue;
            }
            let pixel = if is_snowflake(dx, dy, r) { [255, 255, 255] } else { color };
            let i = ((y * SIZE + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&pixel);
            rgba[i + 3] = (alpha * 255.0) as u8;
        }
    }
    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).expect("图标尺寸固定，必然合法")
}

/// 三条交叉的线段（0°、60°、120°），长度到半径的 70%。
fn is_snowflake(dx: f32, dy: f32, r: f32) -> bool {
    if r > SIZE as f32 * 0.35 {
        return false;
    }
    [0.0f32, 60.0, 120.0].iter().any(|deg| {
        let (s, c) = deg.to_radians().sin_cos();
        // 点到过原点、方向为 (c, s) 的直线的距离。
        (dx * s - dy * c).abs() < 1.6
    })
}
