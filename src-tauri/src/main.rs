// 阻止 Windows 上 release 构建弹出额外的控制台窗口（请勿删除）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    deeprein_lib::run()
}
