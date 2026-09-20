//! 本地持久化。两个文件都在 `%APPDATA%\desktop-task-queue\` 下，
//! 不依赖当前工作目录，从桌面快捷方式或 `target\release` 直接启动都指向同一份数据：
//!
//! - `state.json`：当前未完成的队列 + 窗口状态，临时文件 + 原子替换整体覆写
//! - `done.jsonl`：完成记录，只追加不回读，用来事后翻「今天干了些啥」

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write as _};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredTask {
    pub id: u64,
    pub text: String,
}

/// 窗口几何。字段名沿用旧版，老存档可以直接读。
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct WindowState {
    /// 收起入口的左上角 x，每次启动都会按当前显示器右边缘重新校正。
    pub x: f32,
    /// 收起入口的纵向位置，用户拖动后持久化。
    pub y: f32,
    /// 展开后的面板宽度。
    pub width: f32,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 200.0,
            width: 320.0,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct AppState {
    #[serde(default)]
    pub tasks: Vec<StoredTask>,
    #[serde(default)]
    pub window: Option<WindowState>,
}

fn state_file() -> PathBuf {
    let dir = std::env::var("APPDATA").map_or_else(|_| PathBuf::from("."), PathBuf::from);
    dir.join("desktop-task-queue").join("state.json")
}

pub fn load() -> AppState {
    let path = state_file();
    let Ok(bytes) = fs::read(&path) else {
        return AppState::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        // 文件损坏：留一份现场，然后全新开始，避免反复读写坏文件。
        let _ = fs::rename(&path, path.with_extension("json.broken"));
        AppState::default()
    })
}

/// 先写 `.tmp` 再 rename 覆盖，避免异常退出留下半截 JSON。
/// Windows 的 `fs::rename` 走 `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`，可直接替换目标。
pub fn save(state: &AppState) -> io::Result<()> {
    let path = state_file();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    fs::rename(&tmp, &path)
}

/// 完成的任务追加一行到 `done.jsonl`。
///
/// 只追加、从不回读，所以它出问题也影响不到主存档；前端行为不变，任务照样立刻消失。
/// 写失败就算了——记流水账不值得打断用户操作。
pub fn log_done(text: &str) {
    let path = state_file().with_file_name("done.jsonl");
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let line = serde_json::json!({ "done_at": crate::win::local_now(), "text": text });
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}
