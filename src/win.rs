//! Windows 专用代码，全项目仅此一处。
//!
//! - `claim_single_instance`：防多开
//! - `autostart_enabled` / `set_autostart`：开机启动开关
//! - `watch_topmost`：看门狗线程，定期把窗口重新抬回置顶带顶部
//! - `local_now`：本地时间，给完成流水账打时间戳
//!
//! 非 Windows 平台给的是空实现/回退，只为了还能编过；这个程序本来就是 Windows-first。

use std::time::Duration;

/// 重新申明置顶的心跳间隔。
const RAISE_INTERVAL: Duration = Duration::from_secs(2);

/// Rust 字符串转成 Win32 要的 UTF-16 + NUL 结尾缓冲区。
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 抢占"本会话唯一实例"的标记。返回 `false` 表示已经有一个在跑了。
///
/// 用命名互斥量而不是锁文件：进程不管是正常退出还是崩溃，内核都会自动释放这个内核对象，
/// 不会留下需要人工清理的陈旧锁。名字带 `Local` 前缀，按登录会话隔离 —— 多用户/远程桌面
/// 各自一份，互不干扰。
///
/// 拿到的句柄**故意不关闭**：命名对象只要还有句柄打开就存在，得让它活到进程结束。
#[cfg(windows)]
pub fn claim_single_instance() -> bool {
    const ERROR_ALREADY_EXISTS: u32 = 183;

    let name = wide(r"Local\desktop-task-queue-single-instance");
    // GetLastError 必须紧跟着调用，中间不能插任何别的调用。
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    let err = unsafe { GetLastError() };

    if handle == 0 {
        return true; // 建不出来就别拦着用户，最多退化成没有防多开
    }
    err != ERROR_ALREADY_EXISTS
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(attrs: *const core::ffi::c_void, initial_owner: i32, name: *const u16)
        -> isize;
    fn GetLastError() -> u32;
}

/// 开机启动登记在 HKCU 的 Run 键下。
///
/// 为什么用注册表而不是「启动文件夹放快捷方式」：后者得用 COM 的 IShellLink 才能建 .lnk，
/// 多一大段代码；而 Run 键的条目照样会出现在任务管理器的「启动应用」里，用户随时能在那关掉。
/// 也没必要上计划任务 —— 那能做延迟启动，但要多维护一个系统对象，对一个 7MB 的挂件不值。
#[cfg(windows)]
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE: &str = "desktop-task-queue";

#[cfg(windows)]
fn open_run_key(access: u32) -> Option<isize> {
    // 预定义句柄是 0x80000001 先转 LONG 再转指针，所以要走 i32 做符号扩展。
    const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;

    let sub = wide(RUN_SUBKEY);
    let mut key: isize = 0;
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, access, &mut key) };
    (rc == 0).then_some(key)
}

/// Run 键里有没有登记我们。纯读，无副作用。
#[cfg(windows)]
pub fn autostart_enabled() -> bool {
    const KEY_READ: u32 = 0x2_0019;

    let Some(key) = open_run_key(KEY_READ) else {
        return false;
    };
    let name = wide(RUN_VALUE);
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    unsafe { RegCloseKey(key) };
    rc == 0
}

/// 登记 / 注销开机启动，返回是否真的落到了注册表上。
///
/// 写的是当前 exe 的绝对路径并加引号（路径可能含空格）。每次启用都按当前 exe 重写，
/// 所以 exe 换了位置（比如重新拷到桌面）跑一次就自动把登记的路径校正过来。
#[cfg(windows)]
pub fn set_autostart(on: bool) -> bool {
    const KEY_QUERY_VALUE: u32 = 0x1;
    const KEY_SET_VALUE: u32 = 0x2;
    const REG_SZ: u32 = 1;
    const ERROR_FILE_NOT_FOUND: i32 = 2;

    let Some(key) = open_run_key(KEY_SET_VALUE | KEY_QUERY_VALUE) else {
        return false;
    };
    let name = wide(RUN_VALUE);

    let rc = if on {
        let Ok(exe) = std::env::current_exe() else {
            unsafe { RegCloseKey(key) };
            return false;
        };
        let cmd = wide(&format!(r#""{}""#, exe.display()));
        unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                cmd.as_ptr().cast(),
                // 字节数，含结尾的 NUL
                (cmd.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteValueW(key, name.as_ptr()) }
    };
    unsafe { RegCloseKey(key) };

    // 关的时候本来就不存在，也算达成目的
    rc == 0 || (!on && rc == ERROR_FILE_NOT_FOUND)
}

#[cfg(windows)]
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: isize,
        sub: *const u16,
        options: u32,
        access: u32,
        out: *mut isize,
    ) -> i32;
    fn RegQueryValueExW(
        key: isize,
        name: *const u16,
        reserved: *mut u32,
        ty: *mut u32,
        data: *mut u8,
        len: *mut u32,
    ) -> i32;
    fn RegSetValueExW(
        key: isize,
        name: *const u16,
        reserved: u32,
        ty: u32,
        data: *const u8,
        len: u32,
    ) -> i32;
    fn RegDeleteValueW(key: isize, name: *const u16) -> i32;
    fn RegCloseKey(key: isize) -> i32;
}

/// 起一个看门狗线程，周期性把窗口重新抬回置顶带的顶部；窗口销毁后线程自行退出。
///
/// 为什么需要它：创建窗口时的 always-on-top 只保证进入置顶带，**不保证在带内排第一**。
/// 带内次序按最后一次被抬起的时间排，别的置顶窗口（截图工具、播放器置顶、IM 悬浮提醒、
/// 录屏条、显卡 overlay）一被激活就会压在上面，而且会一直待在上面，直到有东西重排 z-order
/// —— 用户"切回桌面点一下"就是在手动触发这个重排。
///
/// 为什么框架层做不到：`ViewportCommand::WindowLevel` 会走到 winit 的 `apply_diff`，
/// 而它在 window level 没变化时直接 return，重复下发是空操作。
///
/// 为什么必须是独立线程而不是 UI 线程的定时器：窗口被**完全盖住**时 eframe 认为它不可见
/// （`ViewportInfo::visible()` 把 occluded 也算进去），于是只跑 `App::logic`、不跑 `App::ui`，
/// 并且丢掉 repaint 延时直接 `EventResult::Wait`。也就是说 UI 线程的心跳恰好在唯一需要它的
/// 场景里停摆。独立线程不受渲染循环调度影响。
#[cfg(windows)]
pub fn watch_topmost(hwnd: isize) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(RAISE_INTERVAL);
            if !is_window(hwnd) {
                break; // 窗口没了就收工，免得 HWND 被系统回收复用后误伤别人的窗口
            }
            // 全屏游戏/视频/放映时让位，退出后下一次心跳自动恢复。
            if !system_busy() {
                keep_on_top(hwnd);
            }
        }
    });
}

/// `SWP_NOACTIVATE` 保证只改 z-order、不碰焦点 —— Topmost != Always focused。
/// `SWP_ASYNCWINDOWPOS` 是从非属主线程调用时的正确做法，不会被 UI 线程阻塞。
#[cfg(windows)]
fn keep_on_top(hwnd: isize) {
    const HWND_TOPMOST: isize = -1;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_ASYNCWINDOWPOS: u32 = 0x4000;

    unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

/// 系统是否处于"别打扰"状态：全屏 D3D、演示模式，或有程序请求了免打扰。
///
/// 用来在全屏游戏、全屏视频、PPT 放映期间让位 —— 一个 36px 的方块画在游戏画面上很讨厌，
/// 还可能把游戏踢出独占全屏。
#[cfg(windows)]
fn system_busy() -> bool {
    const QUNS_BUSY: i32 = 2;
    const QUNS_RUNNING_D3D_FULL_SCREEN: i32 = 3;
    const QUNS_PRESENTATION_MODE: i32 = 4;

    let mut state = 0;
    if unsafe { SHQueryUserNotificationState(&mut state) } != 0 {
        return false; // 查不到就当没在忙，照常置顶
    }
    matches!(
        state,
        QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
    )
}

#[cfg(windows)]
fn is_window(hwnd: isize) -> bool {
    unsafe { IsWindow(hwnd) != 0 }
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn SetWindowPos(
        hwnd: isize,
        insert_after: isize,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    fn IsWindow(hwnd: isize) -> i32;
}

#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHQueryUserNotificationState(state: *mut i32) -> i32;
}

/// 本地时间 `YYYY-MM-DD HH:MM`。
///
/// std 只给得到 UTC 时间戳，拿不到时区偏移。为了一个时间戳背一整个日期库不划算，
/// 这里直接向系统要一次本地时间。
#[cfg(windows)]
pub fn local_now() -> String {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(out: *mut SystemTime);
    }

    let mut t = SystemTime::default();
    unsafe { GetLocalTime(&mut t) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute
    )
}

#[cfg(not(windows))]
pub fn claim_single_instance() -> bool {
    true
}

#[cfg(not(windows))]
pub fn autostart_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_autostart(_on: bool) -> bool {
    false
}

#[cfg(not(windows))]
pub fn watch_topmost(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn local_now() -> String {
    // 非 Windows 上退回 UTC 时间戳；这个程序本来就是 Windows-first。
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|_| "?".to_owned(), |d| d.as_secs().to_string())
}
