//! 应用主体：单列任务队列，完成即删除。
//!
//! 窗口有两个形态，共用同一个 native window：
//! - collapsed：贴在显示器右缘的小方块，只显示未完成数量，长期常驻
//! - expanded：完整任务板，从右向左展开，用户主动点击入口才出现
//!
//! 位置和尺寸由 `apply_geometry` 每帧统一下发，窗口本身不可拖动、不可缩放，
//! 因此不会和系统的 Aero Snap 打架。
//!
//! 唯一需要 Windows API 的是置顶：创建时的 always-on-top 保不住带内次序，得靠
//! `RAISE_INTERVAL` 的低频心跳重新申明，见 `win::keep_on_top`。

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, FontId, Id, Margin, Pos2, Rect, Sense, Stroke, ViewportCommand};

use crate::storage::{self, AppState, StoredTask, WindowState};
use crate::win;

const PAD: Margin = Margin::symmetric(10, 6);
pub const HANDLE_W: f32 = 36.0; // 收起入口尺寸
pub const HANDLE_H: f32 = 36.0;
const BULLET_W: f32 = 20.0; // 行左侧完成按钮宽度
const ROW_MIN_H: f32 = 26.0;
const ROW_PAD_Y: f32 = 6.0;
const MAX_LIST_H: f32 = 500.0;
const MIN_WIN_H: f32 = 56.0;
const MAX_WIN_H: f32 = 600.0;
const FADE: f32 = 0.15; // 完成动画时长（秒）
const SAVE_DELAY: Duration = Duration::from_millis(400);

const BG: Color32 = Color32::from_rgb(30, 30, 34);
const TEXT: Color32 = Color32::from_gray(214);
const MUTED: Color32 = Color32::from_gray(150);
const SEP: Color32 = Color32::from_gray(52);
const ACCENT: Color32 = Color32::from_gray(90); // 入口左侧那道细边

fn fade(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

struct Task {
    id: u64,
    text: String,
    done: bool, // 已点完成，正在淡出；动画结束即删除
}

pub struct TaskQueue {
    tasks: Vec<Task>,
    next_id: u64,
    input: String,
    drag: Option<u64>, // 正在拖动排序的任务 id
    dirty: bool,
    last_save: Instant,
    expanded: bool,
    handle_x: f32,                    // 收起入口左上角，x 每帧按显示器右缘校正
    handle_y: f32,                    // 用户上下拖动入口改的就是它
    width: f32,                       // 展开宽度
    screen_h: f32,                    // 缓存的显示器高度，只用可信读数刷新
    last_monitor: Option<egui::Vec2>, // 上一帧的显示器读数，用于两帧一致性校验
    grab_dy: f32,                     // 拖动入口时抓取点距窗口顶部的偏移
    saw_focus: bool,                  // 展开后是否已拿到过系统焦点，避免刚展开就被判失焦
    focus_input: bool,
    watching: bool,  // 置顶看门狗是否已启动
    autostart: bool, // 开机启动是否已登记
}

impl TaskQueue {
    pub fn new(cc: &eframe::CreationContext<'_>, state: AppState) -> Self {
        set_fonts(&cc.egui_ctx);

        // 固定深色：默认跟随系统主题，浅色系统下右键菜单会变成白底。
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = BG;
        visuals.extreme_bg_color = BG;
        visuals.override_text_color = Some(TEXT);
        cc.egui_ctx.set_visuals(visuals);

        let next_id = state.tasks.iter().map(|t| t.id + 1).max().unwrap_or(1);
        let w = state.window.unwrap_or_default();

        let autostart = win::autostart_enabled();
        if autostart {
            // exe 可能被挪过位置（比如重新拷到桌面），顺手把登记的路径校准到当前 exe
            win::set_autostart(true);
        }
        Self {
            tasks: state
                .tasks
                .into_iter()
                .map(|t| Task {
                    id: t.id,
                    text: t.text,
                    done: false,
                })
                .collect(),
            next_id,
            input: String::new(),
            drag: None,
            dirty: false,
            last_save: Instant::now(),
            expanded: false,
            handle_x: w.x,
            handle_y: w.y.max(0.0),
            width: w.width,
            screen_h: f32::INFINITY,
            last_monitor: None,
            grab_dy: 0.0,
            saw_focus: false,
            focus_input: false,
            watching: false,
            autostart,
        }
    }

    /// 展开状态的任务板，返回内容需要的高度（不含外边距）。
    ///
    /// 高度不能直接量 `ui` 的实际占用：`ScrollArea` 会被当前窗口高度反向约束，
    /// 那样窗口永远长不起来。这里用 `content_size` 拿到不受约束的内容高度。
    fn panel(&mut self, ui: &mut egui::Ui) -> f32 {
        let top = ui.cursor().min.y;

        // 空白处右键 -> Exit。收起状态窗口只有 36px，放不下弹出菜单，
        // 所以退出入口只挂在展开面板上。放在最前面，后面的控件会盖住它。
        // 勾选状态在闭包里改本地变量，出来再提交，避开对 self 的借用。
        let mut want_autostart = self.autostart;
        ui.interact(ui.max_rect(), Id::new("panel_bg"), Sense::click())
            .context_menu(|ui| {
                ui.checkbox(&mut want_autostart, "Start with Windows");
                ui.separator();
                if ui.button("Exit").clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                    ui.close();
                }
            });
        // 注册表没写成就不改内存状态，勾选框下一帧自己弹回去
        if want_autostart != self.autostart && win::set_autostart(want_autostart) {
            self.autostart = want_autostart;
        }

        // 右上角把入口本体留在原位：还是那个数字，再点一下就收起。
        let head = ui.available_rect_before_wrap();
        let tab = Rect::from_min_size(
            Pos2::new(head.right() - HANDLE_W, head.top() - 2.0),
            egui::vec2(HANDLE_W, 22.0),
        );
        let tab_resp = ui.interact(tab, Id::new("collapse"), Sense::click());
        let collapse = tab_resp.clicked();
        let n = self.pending();
        if tab_resp.hovered() {
            ui.painter()
                .rect_filled(tab, 4.0, Color32::from_rgba_unmultiplied(255, 255, 255, 12));
        }
        ui.painter().text(
            tab.center(),
            egui::Align2::CENTER_CENTER,
            n,
            FontId::proportional(14.0),
            if tab_resp.hovered() { TEXT } else { MUTED },
        );

        // 输入框：Enter 添加。
        let input = ui.add(
            egui::TextEdit::singleline(&mut self.input)
                .hint_text("+ Add a task...")
                .font(FontId::proportional(14.0))
                .desired_width((head.width() - HANDLE_W - 6.0).max(40.0))
                .frame(egui::Frame::NONE),
        );
        if self.focus_input {
            self.focus_input = false;
            input.request_focus();
        }
        if input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let text = std::mem::take(&mut self.input).trim().to_owned();
            if !text.is_empty() {
                self.tasks.push(Task {
                    id: self.next_id,
                    text,
                    done: false,
                });
                self.next_id += 1;
                self.dirty = true;
            }
            input.request_focus();
        }

        ui.add_space(5.0);
        let y = ui.cursor().min.y;
        ui.painter()
            .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, SEP));
        ui.add_space(5.0);

        if collapse {
            self.collapse();
        }

        let head_h = ui.cursor().min.y - top;
        let list = egui::ScrollArea::vertical()
            .max_height(MAX_LIST_H)
            .auto_shrink([false, true])
            .show(ui, |ui| self.rows(ui));

        head_h + list.content_size.y.min(MAX_LIST_H)
    }

    fn rows(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 0.0; // 行距做进行高里，收起动画才平滑

        let font = FontId::proportional(14.0);
        let width = ui.available_width();
        let wrap = (width - BULLET_W - 4.0).max(40.0);
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let clip = ui.clip_rect();

        let mut rects = Vec::with_capacity(self.tasks.len());
        let mut dead = Vec::new();
        let mut finished = None;
        let mut grabbed = None;

        for task in &self.tasks {
            // 完成动画：1 -> 0，同时驱动透明度和行高。
            let t = ui
                .ctx()
                .animate_bool_with_time(Id::new(("fade", task.id)), !task.done, FADE);
            let galley =
                ui.painter()
                    .layout(task.text.clone(), font.clone(), Color32::PLACEHOLDER, wrap);
            let h = (galley.size().y + ROW_PAD_Y * 2.0).max(ROW_MIN_H) * t;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, h), Sense::hover());
            rects.push(rect);
            if task.done && t <= 0.0 {
                dead.push(task.id);
            }
            if h < 0.5 {
                continue;
            }

            // 左侧圆圈点击完成，右侧整条作为拖动手柄，两者不重叠，避免误触。
            let live = !task.done;
            let sense = |s| if live { s } else { Sense::hover() };
            let bullet = ui.interact(
                Rect::from_min_size(rect.min, egui::vec2(BULLET_W, rect.height())),
                Id::new(("done", task.id)),
                sense(Sense::click()),
            );
            let row = ui.interact(
                rect.with_min_x(rect.left() + BULLET_W),
                Id::new(("drag", task.id)),
                sense(Sense::drag()),
            );
            if bullet.clicked() {
                finished = Some(task.id);
            }
            if row.drag_started() {
                grabbed = Some(task.id);
            }

            let p = ui.painter().with_clip_rect(rect.intersect(clip));
            let dragging = self.drag == Some(task.id);
            if dragging || bullet.hovered() || row.hovered() {
                let a = if dragging { 18 } else { 10 };
                p.rect_filled(rect, 4.0, Color32::from_rgba_unmultiplied(255, 255, 255, a));
            }
            let center = Pos2::new(rect.left() + BULLET_W * 0.5, rect.center().y);
            let ring = if bullet.hovered() { TEXT } else { MUTED };
            p.circle_stroke(center, 4.5, Stroke::new(1.2, fade(ring, t)));
            if task.done {
                p.circle_filled(center, 4.5 * t, fade(MUTED, t));
            }
            p.galley(
                Pos2::new(
                    rect.left() + BULLET_W,
                    rect.center().y - galley.size().y * 0.5,
                ),
                galley,
                fade(TEXT, t),
            );
        }

        if let Some(id) = finished {
            if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
                task.done = true;
                self.dirty = true; // 立刻从存档里去掉，动画中途退出也不会复活
                storage::log_done(&task.text); // 队列里没了，但流水账里留一条
            }
        }
        if grabbed.is_some() {
            self.drag = grabbed;
        }
        if !ui.input(|i| i.pointer.any_down()) {
            self.drag = None;
        }

        // 拖拽排序：越过相邻行中线就交换，列表实时跟随指针。
        // 循环到位而不是一帧只挪一格，掉帧时也不会落在指针后面；
        // 每次交换都朝指针所在的槽位靠近一格，因此必然收敛。
        if let (Some(id), Some(p)) = (self.drag, pointer) {
            while let Some(i) = self.tasks.iter().position(|t| t.id == id) {
                if i > 0 && p.y < rects[i - 1].center().y {
                    self.tasks.swap(i - 1, i);
                } else if i + 1 < rects.len() && p.y > rects[i + 1].center().y {
                    self.tasks.swap(i, i + 1);
                } else {
                    break;
                }
                self.dirty = true;
            }
        }

        if !dead.is_empty() {
            self.tasks.retain(|t| !dead.contains(&t.id));
        }
    }

    fn pending(&self) -> usize {
        self.tasks.iter().filter(|t| !t.done).count()
    }

    fn expand(&mut self, ctx: &egui::Context) {
        self.expanded = true;
        self.saw_focus = false;
        self.focus_input = true;
        // 只在用户点击入口时要一次焦点，好让输入框能直接打字；不是持续抢前台。
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn collapse(&mut self) {
        self.expanded = false;
        self.saw_focus = false;
        self.drag = None;
    }

    /// 收起状态：贴着显示器右缘的小方块，显示未完成数量。
    /// 点击展开，上下拖动改变纵向位置。
    fn handle_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let resp = ui.interact(rect, Id::new("handle"), Sense::click_and_drag());

        if let Some(p) = resp.interact_pointer_pos() {
            if resp.drag_started() {
                self.grab_dy = p.y;
            }
            if resp.dragged() {
                // p 是窗口内坐标；窗口会跟着走，所以要换算回屏幕坐标再减抓取偏移。
                // 起始帧 p.y == grab_dy，位移为零，不会跳一下。
                let win_y = ui
                    .ctx()
                    .input(|i| i.viewport().outer_rect)
                    .map_or(0.0, |r| r.min.y);
                self.handle_y = win_y + p.y - self.grab_dy;
                self.dirty = true;
            }
        }
        if resp.clicked() {
            self.expand(ui.ctx());
        }

        let n = self.pending();
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, 0.0, Color32::from_rgb(44, 44, 50));
        }
        p.vline(rect.left() + 0.5, rect.y_range(), Stroke::new(1.0, ACCENT));
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            n,
            FontId::proportional(15.0),
            if n == 0 { MUTED } else { TEXT },
        );
    }

    /// 每帧统一下发窗口位置与尺寸：右缘贴屏幕边，y 用入口位置，尺寸按形态定。
    fn apply_geometry(&mut self, ctx: &egui::Context, content_h: f32) {
        let (outer, inner, monitor) = ctx.input(|i| {
            let v = i.viewport();
            (v.outer_rect, v.inner_rect, v.monitor_size)
        });
        let (Some(outer), Some(inner)) = (outer, inner) else {
            return;
        };

        // egui 只给得到当前显示器的尺寸、给不到原点，按原点 x=0 处理（主显示器正确）。
        //
        // 这个尺寸是 winit 用当时的 pixels_per_point 换算出来的，窗口改尺寸/移动的
        // 那几帧会抖（本机主屏 150%、副屏 200%，同一块屏先后报过 1706.7 和 1280）。
        // 一个坏读数就能把入口钉到屏幕外，所以只在窗口完全静止（收起且尺寸已就位）
        // 且连续两帧读数一致时才采信；读数一直不可信就沿用上次的贴边位置。
        let steady = !self.expanded
            && (inner.width() - HANDLE_W).abs() < 0.5
            && (inner.height() - HANDLE_H).abs() < 0.5;
        let trusted = monitor.filter(|m| steady && Some(*m) == self.last_monitor);
        self.last_monitor = monitor;
        if let Some(m) = trusted {
            self.screen_h = m.y;
            let x = m.x - HANDLE_W;
            let y = self.handle_y.clamp(0.0, (m.y - HANDLE_H).max(0.0));
            if (x - self.handle_x).abs() > 0.5 || (y - self.handle_y).abs() > 0.5 {
                self.handle_x = x;
                self.handle_y = y;
                self.dirty = true;
            }
        }

        let (w, h) = if self.expanded {
            (self.width, content_h.clamp(MIN_WIN_H, MAX_WIN_H))
        } else {
            (HANDLE_W, HANDLE_H)
        };
        // 右缘钉在入口右边，向左展开；整块不越出屏幕下缘。
        let want = Pos2::new(
            self.handle_x + HANDLE_W - w,
            self.handle_y.min((self.screen_h - h).max(0.0)),
        );

        let resize = (inner.width() - w).abs() > 0.5 || (inner.height() - h).abs() > 0.5;
        let reposition = (outer.min.x - want.x).abs() > 0.5 || (outer.min.y - want.y).abs() > 0.5;

        // 顺序很关键。入口右缘就贴着显示器边界，如果先放大再左移，窗口会有一瞬
        // 越界到右边那块屏上；只要相邻屏的缩放比例不同，Windows 立刻发 WM_DPICHANGED，
        // 之后这一帧剩下的命令就按新比例换算，窗口直接被甩到另一块屏回不来。
        // 所以：变宽时先左移再放大，变窄时先缩小再右移，中间态永远不越界。
        let send_size = || {
            if resize {
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(w, h)));
            }
        };
        let send_pos = || {
            if reposition {
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(want));
            }
        };
        if w <= inner.width() {
            send_size();
            send_pos();
        } else {
            send_pos();
            send_size();
        }
    }

    fn autosave(&mut self, ctx: &egui::Context) {
        if !self.dirty {
            return;
        }
        if ctx.input(|i| i.pointer.any_down()) {
            return; // 拖动过程中不写盘
        }
        let wait = SAVE_DELAY.saturating_sub(self.last_save.elapsed());
        if wait.is_zero() {
            self.save_now();
        } else {
            ctx.request_repaint_after(wait);
        }
    }

    fn save_now(&mut self) {
        let state = AppState {
            tasks: self
                .tasks
                .iter()
                .filter(|t| !t.done)
                .map(|t| StoredTask {
                    id: t.id,
                    text: t.text.clone(),
                })
                .collect(),
            window: Some(WindowState {
                x: self.handle_x.round(),
                y: self.handle_y.round(),
                width: self.width.round(),
            }),
        };
        if storage::save(&state).is_ok() {
            self.dirty = false;
        }
        self.last_save = Instant::now();
    }
}

impl eframe::App for TaskQueue {
    /// 首帧拿到 HWND 后把置顶看门狗挂上。
    ///
    /// 放在 `logic` 而不是 `ui`：窗口被完全盖住时 eframe 只跑 `logic`、不跑 `ui`。
    fn logic(&mut self, _ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.watching {
            if let Some(hwnd) = hwnd_of(frame) {
                self.watching = true;
                win::watch_topmost(hwnd);
            }
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if self.expanded {
            // Esc 收起。0.36 的 TextEdit 不处理 Escape，编辑输入框时也能正常收起。
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.collapse();
            }
            // 失焦收起：判据是系统窗口焦点，egui 内部控件之间的焦点切换不会误伤，
            // 拖任务、播完成动画期间窗口焦点也不变。刚展开还没拿到焦点时先不判。
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
            if focused {
                self.saw_focus = true;
            } else if self.saw_focus {
                self.collapse();
            }
        }

        let too_narrow = ctx
            .input(|i| i.viewport().inner_rect)
            .is_some_and(|r| r.width() < self.width - 1.0);

        let content_h = if !self.expanded {
            self.handle_ui(ui);
            HANDLE_H
        } else if too_narrow {
            // 窗口还没变宽，这一帧不画内容：按 36px 宽换行会算出离谱的高度。
            ctx.request_repaint();
            MIN_WIN_H
        } else {
            egui::Frame::NONE
                .inner_margin(PAD)
                .show(ui, |ui| self.panel(ui))
                .inner
                + f32::from(PAD.top + PAD.bottom)
        };

        self.apply_geometry(&ctx, content_h);
        self.autosave(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_now();
    }
}

/// 从 eframe 手里拿到本窗口的 HWND。native 下 `eframe::Frame` 实现了 `HasWindowHandle`。
fn hwnd_of(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
        _ => None,
    }
}

/// 挂一个系统中文字体作为回退，不打包字体文件；路径不存在就跳过。
fn set_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in [
        "C:/Windows/Fonts/msyh.ttc",   // 微软雅黑
        "C:/Windows/Fonts/Deng.ttf",   // 等线
        "C:/Windows/Fonts/simhei.ttf", // 黑体
        "C:/Windows/Fonts/simsun.ttc", // 宋体
    ] {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        fonts.font_data.insert(
            "cjk".to_owned(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("cjk".to_owned());
        }
        break;
    }
    ctx.set_fonts(fonts);
}
