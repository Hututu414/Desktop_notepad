#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod storage;
mod win;

fn main() -> eframe::Result {
    // 防多开：两个实例会各写各的 state.json，后写的覆盖先写的，任务真的会丢。
    // 已经有一个在跑就安静退出 —— 入口本来就常驻在屏幕边上，用户看得见它。
    if !win::claim_single_instance() {
        return Ok(());
    }

    let state = storage::load();

    // 启动即收起状态：只开一个入口大小的窗口。
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Task Queue")
        .with_decorations(false) // frameless
        .with_always_on_top()
        // 不可 resize：尺寸完全由应用按形态下发，也顺便躲开 Windows Aero Snap。
        .with_resizable(false)
        // 桌面挂件不占任务栏；入口本身就常驻可见，不需要任务栏按钮。
        .with_taskbar(false)
        .with_inner_size([app::HANDLE_W, app::HANDLE_H]);
    // 上次的位置，避免启动时先出现在屏幕中央再跳到边缘。
    // 坐标离谱（存档损坏、显示器换了）就不要了，让应用首帧自己重新贴边。
    let sane = |v: f32| v.is_finite() && (0.0..32000.0).contains(&v);
    if let Some(w) = state.window.as_ref().filter(|w| sane(w.x) && sane(w.y)) {
        viewport = viewport.with_position([w.x, w.y]);
    }

    eframe::run_native(
        "desktop-task-queue",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(app::TaskQueue::new(cc, state)))),
    )
}
