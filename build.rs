//! 构建脚本：把应用图标嵌入 Windows PE 资源。
//!
//! `embed-resource` 与 `winres` 在跨工具链时问题较多，统一用 `winresource`
//! （cargo 自带的 `rust-lld`/rc 封装），仅在 windows 目标生效。

#[cfg(windows)]
fn main() {
    println!("cargo:rerun-if-changed=icons/icon.ico");
    winresource::WindowsResource::new()
        .set_icon("icons/icon.ico")
        .compile()
        .expect("无法编译图标资源：icons/icon.ico");
}

#[cfg(not(windows))]
fn main() {}
