//! 一键重置（清空 DSH 数据）+ 会话备份 / 恢复。
//!
//! 面向企业内小白同事的「彻底重来」入口：当数字分身被插件/配置搞乱、改不回时，
//! 不必手动删目录，托盘点一项即可把 `$DSH_HOME`（~/.dsh-launcher）里的 DSH 用户数据
//! 全部清空，同时（可选）先备份会话历史，下次启动时询问是否找回。
//!
//! 数据布局（本项目定界，勿删错）：
//! - **DSH 用户数据 = `$DSH_HOME`（默认 ~/.dsh-launcher）**：profiles、sessions、
//!   storages、attachments、skills、.dsh-matrix、settings.yaml、launcher-brand 等。
//!   重置=清空这一层。
//! - **启动器自身数据 = `base_dir()`（%APPDATA%\io.github.hairyf.deepseek-harness-launcher）**：
//!   launcher-config.json、dependencies/（node/pnpm/dsh 本体）、logs、**backups/**。
//!   这一层**不清**——应用本体与依赖保留，重启即可重装/找回会话。
//!
//! 因此「备份」放到 `base_dir()/backups/`（Tauri 持久目录，独立于 $DSH_HOME，
//! 清 DSH 数据不会误删，重装应用后仍在），天然满足「全清后还能找回会话」。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tauri::{AppHandle, Runtime};

/// 备份根目录：`<launcher app_data>/backups/`。
pub fn backups_root<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    crate::config::base_dir(app).join("backups")
}

/// 构造一个带时间戳的备份目录路径（不创建）：`backups/20260919-153000/`。
///
/// 用秒级时间戳即可满足「多个备份不冲突」，无需拉起子进程取可读时间。
fn new_backup_dir<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    backups_root(app).join(ts.to_string())
}

/// 应被备份/恢复的 DSH 用户数据顶层条目（$DSH_HOME 下）。
///
/// 显式列出，绝不盲目复制整个 $DSH_HOME（避免把临时状态、锁文件等一并带走）。
const USER_DATA_ENTRIES: &[&str] = &[
    "profiles",
    "sessions",
    "storages",
    "attachments",
    "skills",
    ".dsh-matrix",
    "launcher-brand",
    "settings.yaml",
    ".credentials.yaml",
    ".anonymous-user-id",
    "client-id",
];

/// 把 `$DSH_HOME` 里的 DSH 用户数据备份到 `backups/<ts>`。
///
/// 返回备份目录。成功后该目录存在，供「启动恢复」与「彻底清空」使用。
pub fn backup_user_data<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &crate::config::LauncherConfig,
) -> Result<PathBuf, String> {
    let home = crate::config::dsh_home(app, cfg);
    let dst = new_backup_dir(app);

    for entry in USER_DATA_ENTRIES {
        let src = home.join(entry);
        if !src.exists() {
            continue;
        }
        let target = dst.join(entry);
        if src.is_dir() {
            copy_dir_all(&src, &target)
                .map_err(|e| format!("备份 {entry} 失败：{e}"))?;
        } else {
            std::fs::create_dir_all(target.parent().expect("备份目标应含父目录"))
                .map_err(|e| format!("创建备份目录失败：{e}"))?;
            std::fs::copy(&src, &target).map_err(|e| format!("备份 {entry} 失败：{e}"))?;
        }
    }
    log::info!("DSH 用户数据备份完成：{} → {}", home.display(), dst.display());
    Ok(dst)
}

/// 递归复制目录（保留目录结构与文件）。
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else if ty.is_symlink() {
            // 符号链接（如 node_modules 的 junction）复制链接本身而非内容
            #[cfg(windows)]
            {
                let target = std::fs::read_link(&from)?;
                std::os::windows::fs::symlink_dir(&target, &to).or_else(|_| {
                    std::os::windows::fs::symlink_file(&target, &to)
                })?;
            }
            #[cfg(not(windows))]
            {
                let target = std::fs::read_link(&from)?;
                std::os::unix::fs::symlink(&target, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 清空 `$DSH_HOME` 里的 DSH 用户数据（保留目录本身）。
///
/// 调用前必须已停 Harness（否则文件被锁/进程占用）。不清 `base_dir()`（应用本体/依赖）。
pub fn clear_dsh_data<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &crate::config::LauncherConfig,
) -> Result<(), String> {
    let home = crate::config::dsh_home(app, cfg);
    for entry in USER_DATA_ENTRIES {
        let p = home.join(entry);
        if p.is_dir() {
            std::fs::remove_dir_all(&p).map_err(|e| format!("清空 {entry} 失败：{e}"))?;
        } else if p.exists() {
            std::fs::remove_file(&p).map_err(|e| format!("清空 {entry} 失败：{e}"))?;
        }
    }
    log::info!("DSH 用户数据已清空：{}", home.display());
    Ok(())
}

/// 检测是否存在可恢复的备份（取最新一个）。返回其目录。
pub fn detect_backup<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    let root = backups_root(app);
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    if dirs.is_empty() {
        return None;
    }
    // 按修改时间取最新，作为「最近一次保留的会话」
    dirs.sort_by_key(|p| p.metadata().map(|m| m.modified().unwrap_or(SystemTime::UNIX_EPOCH)).unwrap_or(SystemTime::UNIX_EPOCH));
    dirs.last().cloned()
}

/// 把备份内容恢复回 `$DSH_HOME`（合并式：不删除目标已有文件，仅覆盖同名）。
pub fn restore_backup<R: Runtime>(
    app: &AppHandle<R>,
    backup: &Path,
    cfg: &crate::config::LauncherConfig,
) -> Result<(), String> {
    let home = crate::config::dsh_home(app, cfg);
    if !backup.is_dir() {
        return Err(format!("备份目录不存在：{}", backup.display()));
    }
    for entry in std::fs::read_dir(backup).map_err(|e| format!("读取备份失败：{e}"))? {
        let entry = entry.map_err(|e| format!("读取备份条目失败：{e}"))?;
        let name = entry.file_name();
        let target = home.join(&name);
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            copy_dir_all(&entry.path(), &target)
                .map_err(|e| format!("恢复 {} 失败：{e}", name.to_string_lossy()))?;
        } else {
            std::fs::create_dir_all(target.parent().expect("恢复目标应含父目录"))
                .map_err(|e| format!("创建恢复目录失败：{e}"))?;
            std::fs::copy(&entry.path(), &target)
                .map_err(|e| format!("恢复 {} 失败：{e}", name.to_string_lossy()))?;
        }
    }
    log::info!("会话备份已恢复：{} → {}", backup.display(), home.display());
    Ok(())
}

/// 判断 $DSH_HOME 当前是否「没有会话数据」（供启动时判断是否值得询问恢复）。
pub fn has_no_user_data<R: Runtime>(app: &AppHandle<R>, cfg: &crate::config::LauncherConfig) -> bool {
    let home = crate::config::dsh_home(app, cfg);
    !home.join("profiles").exists()
        && !home.join("sessions").exists()
        && !home.join("settings.yaml").exists()
}

/// 重置方式：用户在二次确认框里的三选一。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetChoice {
    /// 先备份会话历史再清空（下次启动可找回）
    BackupThenClear,
    /// 彻底清空（不备份）
    Wipe,
    /// 取消
    Cancel,
}

/// 原生二次确认框，返回三选一（BackupThenClear / Wipe / Cancel）。
///
/// Windows 用 `MessageBoxW`（MB_YESNOCANCEL）：
/// - **是** = 备份会话历史后再清空（安全默认，推荐小白走这个）
/// - **否** = 彻底清空（不保留，适合完全不想要旧数据）
/// - **取消** = 不执行
#[cfg(windows)]
pub fn confirm_reset_choice() -> ResetChoice {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONWARNING, MB_YESNOCANCEL, IDNO, IDYES,
    };
    let title: Vec<u16> = "重置 DeepSeek Harness".encode_utf16().chain(std::iter::once(0)).collect();
    let body: Vec<u16> = format!(
        "此操作会清空数字分身的所有数据（已装插件、配置、会话、附件、技能）。\n\n\
         不会影响程序本体，清空后可重新安装。\n\n\
         请选择：\n\
         · 点击「是」= 先保留一份会话历史备份，下次启动可找回（推荐）\n\
         · 点击「否」= 彻底清空，不保留历史\n\
         · 点击「取消」= 放弃操作"
    )
    .encode_utf16()
    .chain(std::iter::once(0))
    .collect();
    let ret = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_YESNOCANCEL | MB_ICONWARNING,
        )
    };
    if ret == IDYES {
        ResetChoice::BackupThenClear
    } else if ret == IDNO {
        ResetChoice::Wipe
    } else {
        ResetChoice::Cancel
    }
}

/// 非 Windows 平台：无确认框，默认按「备份后清空」处理（桌面端仅面向 Windows）。
#[cfg(not(windows))]
pub fn confirm_reset_choice() -> ResetChoice {
    ResetChoice::BackupThenClear
}

/// 启动时「找回会话」确认框：返回 true=恢复，false=忽略。
///
/// 时机：检测到存在备份、且当前 $DSH_HOME 没有会话数据（即刚重置/重装完）。
#[cfg(windows)]
pub fn confirm_restore() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONQUESTION, MB_YESNO, IDYES,
    };
    let title: Vec<u16> = "找回上次保留的会话".encode_utf16().chain(std::iter::once(0)).collect();
    let body: Vec<u16> = format!(
        "检测到上次重置时保留的会话历史。\n\n\
         是否恢复这些会话（数字分身的数据、聊天记录、已装配置）？\n\n\
         · 点击「是」= 恢复上次的会话历史\n\
         · 点击「否」= 不要恢复，从全新状态开始"
    )
    .encode_utf16()
    .chain(std::iter::once(0))
    .collect();
    let ret = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONQUESTION,
        )
    };
    ret == IDYES
}

/// 非 Windows 平台：不弹确认，直接恢复（桌面端仅面向 Windows）。
#[cfg(not(windows))]
pub fn confirm_restore() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "dsh-reset-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn copy_dir_recurses_and_preserves_files() {
        let src = tmp().join("src");
        let dst = tmp().join("dst");
        std::fs::create_dir_all(src.join("nested/deep")).unwrap();
        let mut f = std::fs::File::create(src.join("nested/hello.txt")).unwrap();
        f.write_all(b"hello reset").unwrap();
        let mut f2 = std::fs::File::create(src.join("nested/deep/a.txt")).unwrap();
        f2.write_all(b"deep").unwrap();

        copy_dir_all(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(dst.join("nested/hello.txt")).unwrap(), "hello reset");
        assert_eq!(std::fs::read_to_string(dst.join("nested/deep/a.txt")).unwrap(), "deep");
        std::fs::remove_dir_all(src.parent().unwrap()).unwrap();
    }

    #[test]
    fn clear_removes_only_listed_entries() {
        let home = tmp().join("home");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::create_dir_all(home.join("attachments")).unwrap();
        std::fs::create_dir_all(home.join("keep-me")).unwrap();
        std::fs::write(home.join("settings.yaml"), "x").unwrap();

        for entry in USER_DATA_ENTRIES {
            let p = home.join(entry);
            if p.is_dir() {
                std::fs::remove_dir_all(&p).unwrap();
            } else if p.exists() {
                std::fs::remove_file(&p).unwrap();
            }
        }

        assert!(!home.join("sessions").exists());
        assert!(!home.join("attachments").exists());
        assert!(!home.join("settings.yaml").exists());
        // 未被清单收录的目录应保留（模拟「只清 DSH 用户数据，不动其它」）
        assert!(home.join("keep-me").exists());
        std::fs::remove_dir_all(home.parent().unwrap()).unwrap();
    }
}
