//! 原生通知封装（tauri-plugin-notification）。

use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

/// 弹出一条原生通知。失败仅告警（通知不可用时不应阻断主流程）。
pub fn notify<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    if let Err(e) = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
    {
        log::warn!("通知发送失败：{e}");
    }
}
