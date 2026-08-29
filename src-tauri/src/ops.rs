//! 操作状态中心：跟踪所有用户交互操作的状态，供托盘动态显示 + 操作窗口实时展示。
//!
//! 每个耗时操作（安装/启动/同步/测速/镜像上传/管理能力等）通过
//! `start_op` 登记，`update_step`/`append_log` 更新进度，`finish_op`/`fail_op` 收尾。
//! 每次变更向窗口前端 `emit("op-update")`，前端监听刷新。

use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Runtime};

/// 操作状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OpState {
    /// 无操作
    Idle,
    /// 进行中
    Running,
    /// 成功完成
    Done,
    /// 失败
    Failed,
}

/// 单个步骤状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed,
}

/// 操作中的一个步骤。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Step {
    pub label: String,
    pub state: StepState,
}

/// 当前操作。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Operation {
    pub id: String,
    pub label: String,
    pub state: OpState,
    /// 当前步骤描述（含进度，如 "正在下载 Node.js 45%"）
    pub current_step: String,
    /// 步骤列表（预置后逐个标记完成）
    pub steps: Vec<Step>,
    /// 实时日志（最近 MAX_LOG_LINES 条）
    pub log: Vec<String>,
    /// 完成/失败详情
    pub result: String,
}

impl Default for Operation {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            state: OpState::Idle,
            current_step: String::new(),
            steps: Vec::new(),
            log: Vec::new(),
            result: String::new(),
        }
    }
}

/// 日志条数上限（防内存膨胀）。
const MAX_LOG_LINES: usize = 200;

static CURRENT: Mutex<Option<Operation>> = Mutex::new(None);

/// 加锁（容忍 poison：panic 后锁被污染不阻断后续）。
fn lock_current() -> std::sync::MutexGuard<'static, Option<Operation>> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner())
}

/// 开始一个操作（自动结束上一个未完成操作）。
pub fn start_op<R: Runtime>(app: &AppHandle<R>, id: &str, label: &str, steps: &[&str]) {
    let op = Operation {
        id: id.to_string(),
        label: label.to_string(),
        state: OpState::Running,
        current_step: "准备中…".to_string(),
        steps: steps.iter().map(|s| Step { label: s.to_string(), state: StepState::Pending }).collect(),
        log: vec![format!("[开始] {label}")],
        result: String::new(),
    };
    *lock_current() = Some(op);
    log::info!("操作开始：{label}");
    persist(app);
    emit_update(app);
}

/// 更新当前步骤描述（如 "正在下载 Node.js 45%"）。
pub fn update_step<R: Runtime>(app: &AppHandle<R>, step: &str) {
    if let Some(op) = lock_current().as_mut() {
        if op.state == OpState::Running {
            op.current_step = step.to_string();
        }
    }
    emit_update(app);
}

/// 标记第 i 个步骤为进行中（前置步骤标记完成）。
pub fn mark_step_running<R: Runtime>(app: &AppHandle<R>, index: usize) {
    if let Some(op) = lock_current().as_mut() {
        if op.state != OpState::Running {
            return;
        }
        for (i, step) in op.steps.iter_mut().enumerate() {
            if i < index {
                step.state = StepState::Done;
            } else if i == index {
                step.state = StepState::Running;
            } else {
                step.state = StepState::Pending;
            }
        }
    }
    emit_update(app);
}

/// 标记第 i 个步骤失败。
pub fn mark_step_failed<R: Runtime>(app: &AppHandle<R>, index: usize) {
    if let Some(op) = lock_current().as_mut() {
        if let Some(step) = op.steps.get_mut(index) {
            step.state = StepState::Failed;
        }
    }
    emit_update(app);
}

/// 追加一条日志（带时间戳）。
pub fn append_log<R: Runtime>(app: &AppHandle<R>, line: &str) {
    if let Some(op) = lock_current().as_mut() {
        op.log.push(line.to_string());
        if op.log.len() > MAX_LOG_LINES {
            let excess = op.log.len() - MAX_LOG_LINES;
            op.log.drain(..excess);
        }
    }
    log::info!("{line}");
    emit_update(app);
}

/// 操作成功完成。
pub fn finish_op<R: Runtime>(app: &AppHandle<R>, result: &str) {
    if let Some(op) = lock_current().as_mut() {
        op.state = OpState::Done;
        op.current_step = "完成".to_string();
        op.result = result.to_string();
        for step in op.steps.iter_mut() {
            if step.state == StepState::Running {
                step.state = StepState::Done;
            }
        }
        op.log.push(format!("[完成] {result}"));
    }
    log::info!("操作完成：{result}");
    persist(app);
    emit_update(app);
}

/// 操作失败。
pub fn fail_op<R: Runtime>(app: &AppHandle<R>, error: &str) {
    if let Some(op) = lock_current().as_mut() {
        op.state = OpState::Failed;
        op.current_step = "失败".to_string();
        op.result = error.to_string();
        // 当前进行中的步骤标记为失败（前置已完成的保持完成）
        let mut seen_running = false;
        for step in op.steps.iter_mut() {
            if step.state == StepState::Running {
                step.state = StepState::Failed;
                seen_running = true;
            } else if step.state == StepState::Pending && !seen_running {
                step.state = StepState::Failed;
            }
        }
        op.log.push(format!("[失败] {error}"));
    }
    log::error!("操作失败：{error}");
    persist(app);
    emit_update(app);
}

/// 读取当前操作（无操作返回 None）。
pub fn current() -> Option<Operation> {
    lock_current().clone()
}

/// 是否有进行中的操作。
pub fn has_running() -> bool {
    lock_current()
        .as_ref()
        .map(|op| op.state == OpState::Running)
        .unwrap_or(false)
}

/// 状态落盘路径（`<dsh_home>/ops-state.json`）。
fn state_path<R: Runtime>(app: &AppHandle<R>) -> std::path::PathBuf {
    crate::config::dsh_home(app, &crate::config::load_cached()).join("ops-state.json")
}

/// 落盘当前操作状态（start/finish/fail 时调用，重启后可见上次结果）。
fn persist<R: Runtime>(app: &AppHandle<R>) {
    let Some(op) = current() else { return };
    let path = state_path(app);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&op) {
        let _ = std::fs::write(path, json);
    }
}

/// 启动时从磁盘恢复上次操作状态（供托盘/窗口显示上次结果）。
pub fn load_from_disk<R: Runtime>(app: &AppHandle<R>) {
    let path = state_path(app);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    if let Ok(op) = serde_json::from_str::<Operation>(&text) {
        // 上次 Running 的操作（进程被强杀）视为失败，避免误导
        let op = if op.state == OpState::Running {
            Operation { state: OpState::Failed, current_step: "上次未完成（进程中断）".to_string(), result: "进程中断，操作未完成".to_string(), ..op }
        } else {
            op
        };
        *lock_current() = Some(op);
    }
}

/// 向窗口前端推送更新事件。
fn emit_update<R: Runtime>(app: &AppHandle<R>) {
    let op = current();
    let _ = app.emit("op-update", op);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 串行锁：ops 用全局静态 Mutex，测试需串行避免互相污染。
    /// 容忍 poison（测试 panic 后锁被污染，继续用即可）。
    fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fake_app() -> AppHandle<tauri::test::MockRuntime> {
        tauri::test::mock_app().handle().clone()
    }

    #[test]
    fn state_machine_transitions() {
        let _guard = serial_lock();
        let app = fake_app();
        start_op(&app, "test", "测试操作", &["步骤A", "步骤B"]);
        let op = current().unwrap();
        assert_eq!(op.state, OpState::Running);
        assert_eq!(op.steps.len(), 2);
        assert_eq!(op.steps[0].state, StepState::Pending);

        mark_step_running(&app, 0);
        let op = current().unwrap();
        assert_eq!(op.steps[0].state, StepState::Running);

        update_step(&app, "正在执行步骤A 50%");
        assert_eq!(current().unwrap().current_step, "正在执行步骤A 50%");

        finish_op(&app, "成功");
        let op = current().unwrap();
        assert_eq!(op.state, OpState::Done);
        assert_eq!(op.result, "成功");
        assert_eq!(op.steps[0].state, StepState::Done);
    }

    #[test]
    fn fail_marks_running_step_failed() {
        let _guard = serial_lock();
        let app = fake_app();
        start_op(&app, "t", "测试", &["A", "B"]);
        mark_step_running(&app, 1);
        fail_op(&app, "出错了");
        let op = current().unwrap();
        assert_eq!(op.state, OpState::Failed);
        assert_eq!(op.steps[1].state, StepState::Failed);
        assert_eq!(op.steps[0].state, StepState::Done); // 前置步骤已完成
    }

    #[test]
    fn log_capped_at_limit() {
        let _guard = serial_lock();
        let app = fake_app();
        start_op(&app, "t", "测试", &[]);
        for i in 0..250 {
            append_log(&app, &format!("line {i}"));
        }
        let op = current().unwrap();
        assert!(op.log.len() <= MAX_LOG_LINES, "日志应被截断到 {} 条", MAX_LOG_LINES);
        // 保留的是最新日志
        assert!(op.log.last().unwrap().contains("line 249"));
    }

    #[test]
    fn serializable_for_window() {
        let _guard = serial_lock();
        let app = fake_app();
        start_op(&app, "install", "安装 / 修复", &["Node"]);
        update_step(&app, "下载中 45%");
        let op = current().unwrap();
        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains("\"state\":\"running\""));
        assert!(json.contains("下载中 45%"));
        assert!(json.contains("\"steps\""));
    }
}
