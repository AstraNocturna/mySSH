fn main() {
    // 图标经 resource.rc 编进 exe，但 tauri-build 只监视 tauri.conf.json 与 capabilities：
    // 单换 icon.ico 不会重跑构建脚本，exe 仍是旧图标
    println!("cargo:rerun-if-changed=icons/icon.ico");
    tauri_build::build()
}
