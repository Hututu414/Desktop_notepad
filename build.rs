//! 把预编译好的 Windows 资源（应用图标）链进 exe。
//!
//! 只影响 exe 在资源管理器 / 快捷方式里的外观，不参与程序逻辑。
//!
//! 用的是提前编好的 `assets/icon.res` 而不是在这里调 `rc.exe`：这样构建时不依赖
//! 装没装 Windows SDK。换图标的步骤见 README。

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-changed=assets/icon.res");
    println!("cargo:rustc-link-arg-bins={dir}/assets/icon.res");
}
