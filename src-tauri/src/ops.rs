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
    /// 本次操作**将要做什么**（如 "dsh-himarket 0.1.7 → 0.1.8"）。
    /// 调用方在 start_op 后用 `set_details` 填写；向导第一步展示给用户，
    /// 让用户点「更新」前就知道会改什么（旧版是黑盒执行，点完不知道在装什么）。
    #[serde(default)]
    pub details: Vec<String>,
    /// 开始时间（本地时间字符串，供历史列表展示）
    #[serde(default)]
    pub started_at: String,
    /// 结束时间（终态时填写；进行中为空字符串）
    #[serde(default)]
    pub finished_at: String,
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
            details: Vec::new(),
            started_at: String::new(),
            finished_at: String::new(),
        }
    }
}

/// 日志条数上限（防内存膨胀）。
const MAX_LOG_LINES: usize = 200;

/// 历史条数上限（防内存膨胀；落盘后可跨重启回看）。
pub const MAX_HISTORY: usize = 20;

static CURRENT: Mutex<Option<Operation>> = Mutex::new(None);
/// 已完成操作的归档（最新在前）。
///
/// 旧实现只有一个 `CURRENT` 槽：新操作直接覆盖旧的，用户事后无法回看
/// 「刚才更新了什么、成没成功」——**失败记录尤其会被下一次同步/启动冲掉**
/// （实测：12:03 插件更新失败的记录，被 12:03:49 的 launch 覆盖，事后不可查）。
static HISTORY: Mutex<Vec<Operation>> = Mutex::new(Vec::new());

/// 加锁（容忍 poison：panic 后锁被污染不阻断后续）。
fn lock_current() -> std::sync::MutexGuard<'static, Option<Operation>> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_history() -> std::sync::MutexGuard<'static, Vec<Operation>> {
    HISTORY.lock().unwrap_or_else(|e| e.into_inner())
}

/// 本地时间字符串（历史列表展示用）。
fn now_ts() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 把「当前已结束的操作」移入历史（进行中的不动）。返回是否发生归档。
fn archive_finished() -> bool {
    let finished = {
        let mut cur = lock_current();
        let is_finished = cur.as_ref().map(|op| op.state != OpState::Running).unwrap_or(false);
        if is_finished {
            cur.take()
        } else {
            None
        }
    };
    match finished {
        Some(op) => {
            let mut hist = lock_history();
            hist.insert(0, op);
            hist.truncate(MAX_HISTORY);
            true
        }
        None => false,
    }
}

/// 开始一个操作。
///
/// 若上一个操作已结束，先把它归档进历史再开新的——**不再直接覆盖**
/// （覆盖会丢掉失败记录，用户事后无从回看）。
pub fn start_op<R: Runtime>(app: &AppHandle<R>, id: &str, label: &str, steps: &[&str]) {
    archive_finished();
    let op = Operation {
        id: id.to_string(),
        label: label.to_string(),
        state: OpState::Running,
        current_step: "准备中…".to_string(),
        steps: steps.iter().map(|s| Step { label: s.to_string(), state: StepState::Pending }).collect(),
        log: vec![format!("[开始] {label}")],
        result: String::new(),
        details: Vec::new(),
        started_at: now_ts(),
        finished_at: String::new(),
    };
    *lock_current() = Some(op);
    log::info!("操作开始：{label}");
    persist(app);
    emit_update(app);
}

/// 设置本次操作的「将要做什么」说明（向导第一步展示）。
///
/// 例：`["dsh-himarket：0.1.7 → 0.1.8（来源 npm）"]`。
pub fn set_details<R: Runtime>(app: &AppHandle<R>, details: &[String]) {
    if let Some(op) = lock_current().as_mut() {
        op.details = details.to_vec();
        for d in details {
            op.log.push(format!("[计划] {d}"));
        }
    }
    emit_update(app);
}

/// 读取历史（最新在前）。`limit` 为 0 表示全部。
pub fn history(limit: usize) -> Vec<Operation> {
    let hist = lock_history();
    if limit == 0 || limit >= hist.len() {
        hist.clone()
    } else {
        hist[..limit].to_vec()
    }
}

/// 清空历史。
///
/// 当前由测试使用；保留为公开 API（向导/托盘后续可接「清空历史」入口）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn clear_history<R: Runtime>(app: &AppHandle<R>) {
    lock_history().clear();
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
        op.finished_at = now_ts();
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
        op.finished_at = now_ts();
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
///
/// ⚠️ 单元测试必须走 `set_test_state_path` 覆盖：测试里 `load_cached()` 返回默认配置
/// （`dsh_home = None`），`dsh_home()` 会回落到**用户真实的** `~/.dsh-launcher`，
/// 导致 `cargo test` 把用户线上的 `ops-state.json` 覆盖掉（本轮真实踩到）。
fn state_path<R: Runtime>(app: &AppHandle<R>) -> std::path::PathBuf {
    #[cfg(test)]
    {
        if let Some(p) = lock_test_state_path().clone() {
            return p;
        }
    }
    crate::config::dsh_home(app, &crate::config::load_cached()).join("ops-state.json")
}

/// 测试专用状态路径覆盖（避免单元测试写进用户真实 dsh_home）。
#[cfg(test)]
static TEST_STATE_PATH: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

#[cfg(test)]
fn lock_test_state_path() -> std::sync::MutexGuard<'static, Option<std::path::PathBuf>> {
    TEST_STATE_PATH.lock().unwrap_or_else(|e| e.into_inner())
}

/// 测试专用：把落盘路径指向临时目录（传 None 恢复默认）。
#[cfg(test)]
pub fn set_test_state_path(p: Option<std::path::PathBuf>) {
    *lock_test_state_path() = p;
}

/// 落盘格式：`{ "current": <Operation|null>, "history": [<Operation>...] }`。
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedState {
    #[serde(default)]
    current: Option<Operation>,
    #[serde(default)]
    history: Vec<Operation>,
}

/// 解析落盘内容，兼容旧格式（单个 Operation 对象）。
///
/// ⚠️ 必须**显式**按字段判别，不能靠「反序列化失败就回退」：
/// 旧格式的对象里没有 `current`/`history` 键，而 serde 默认忽略未知字段、
/// 缺失字段又有 `default` 兜底 —— 结果是旧格式会「成功」解析成一个
/// `{current: None, history: []}` 的空状态，**状态被静默丢弃**，回退分支永不触发。
fn parse_state(text: &str) -> Option<(Option<Operation>, Vec<Operation>)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let is_new_format = value.get("current").is_some() || value.get("history").is_some();
    if is_new_format {
        let s: PersistedState = serde_json::from_value(value).ok()?;
        Some((s.current, s.history))
    } else {
        // 旧格式：顶层就是一个 Operation
        let op: Operation = serde_json::from_value(value).ok()?;
        Some((Some(op), Vec::new()))
    }
}

/// 落盘当前操作 + 历史（start/finish/fail/归档时调用，重启后可见）。
fn persist<R: Runtime>(app: &AppHandle<R>) {
    let state = PersistedState {
        current: current(),
        history: history(MAX_HISTORY),
    };
    let path = state_path(app);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&state) {
        let _ = std::fs::write(path, json);
    }
}

/// 启动时从磁盘恢复上次操作状态与历史（供托盘/窗口显示上次结果）。
pub fn load_from_disk<R: Runtime>(app: &AppHandle<R>) {
    let path = state_path(app);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Some((cur, hist)) = parse_state(&text) else { return };

    // 上次 Running 的操作（进程被强杀）视为失败，避免误导
    let normalize = |op: Operation| {
        if op.state == OpState::Running {
            Operation {
                state: OpState::Failed,
                current_step: "上次未完成（进程中断）".to_string(),
                result: "进程中断，操作未完成".to_string(),
                finished_at: if op.finished_at.is_empty() { now_ts() } else { op.finished_at },
                ..op
            }
        } else {
            op
        }
    };

    if let Some(op) = cur {
        *lock_current() = Some(normalize(op));
    }
    if !hist.is_empty() {
        let mut h = lock_history();
        h.clear();
        h.extend(hist.into_iter().take(MAX_HISTORY).map(normalize));
    }
}

/// 测试用：清空全局状态（CURRENT + HISTORY）。
///
/// 测试共享同一组全局静态量，前一个测试留在 CURRENT 里的终态操作会在
/// 下一个测试 `start_op` 时被归档，导致历史条数断言不稳。必须显式重置。
#[cfg(test)]
pub fn reset_for_test() {
    *lock_current() = None;
    lock_history().clear();
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
    ///
    /// ⚠️ 同时把落盘路径重定向到临时目录：否则 `persist()` 会写到用户真实的
    /// `~/.dsh-launcher/ops-state.json`，**测试会破坏线上状态**（本轮真实踩到）。
    fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 每个测试独占一个临时文件，测完即删（不碰用户目录）
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("dsh-launcher-ops-test-{n}.json"));
        let _ = std::fs::remove_file(&p);
        set_test_state_path(Some(p));
        guard
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

    /// 核心回归：新操作开始时，上一个**已失败**的操作必须进历史，不能被覆盖。
    /// 这正是线上事故（12:03 插件更新失败被 12:03:49 的 launch 冲掉）的护栏。
    #[test]
    fn failed_op_is_archived_not_overwritten() {
        let _guard = serial_lock();
        let app = fake_app();
        reset_for_test();

        start_op(&app, "plugin-install", "安装插件", &["安装插件"]);
        fail_op(&app, "PLUGIN_INSTALL_FAILED: dsh-himarket（exit=1）");

        // 后续操作（如启动 Harness）开始 —— 旧实现会把失败记录冲掉
        start_op(&app, "launch", "启动 Harness", &[]);
        finish_op(&app, "已启动");

        let hist = history(0);
        assert_eq!(hist.len(), 1, "失败记录应被归档，实际 {hist:?}");
        assert_eq!(hist[0].id, "plugin-install");
        assert_eq!(hist[0].state, OpState::Failed);
        assert!(hist[0].result.contains("PLUGIN_INSTALL_FAILED"));
        assert!(!hist[0].finished_at.is_empty(), "归档项应有结束时间");
        // 当前操作是新的那个
        assert_eq!(current().unwrap().id, "launch");
    }

    /// 历史有上限，防止无限增长。
    #[test]
    fn history_capped() {
        let _guard = serial_lock();
        let app = fake_app();
        reset_for_test();
        for i in 0..(MAX_HISTORY + 5) {
            start_op(&app, "op", &format!("操作{i}"), &[]);
            finish_op(&app, "ok");
        }
        // 最后一次 finish 后 current 仍是终态；再开一个触发归档
        start_op(&app, "op", "收尾", &[]);
        let hist = history(0);
        assert_eq!(hist.len(), MAX_HISTORY, "历史应被截断到 {MAX_HISTORY} 条");
        // 最新在前
        assert!(hist[0].label.starts_with("操作"), "最新在前，实际 {}", hist[0].label);
    }

    /// 向导第一步：details 必须能带出去（让用户看到「将要更新什么」）。
    #[test]
    fn details_carried_and_logged() {
        let _guard = serial_lock();
        let app = fake_app();
        reset_for_test();
        start_op(&app, "plugin-install", "更新插件", &["更新"]);
        set_details(&app, &["dsh-himarket：0.1.7 → 0.1.8（来源 npm）".to_string()]);
        let op = current().unwrap();
        assert_eq!(op.details.len(), 1);
        assert!(op.details[0].contains("0.1.7 → 0.1.8"));
        // 同时进日志，便于事后回看
        assert!(op.log.iter().any(|l| l.starts_with("[计划]")), "计划应进日志");
    }

    /// 落盘/恢复往返：历史与 details 都要保住（跨重启可回看）。
    #[test]
    fn persist_roundtrip_keeps_history() {
        let _guard = serial_lock();
        let app = fake_app();
        reset_for_test();
        start_op(&app, "plugin-install", "安装插件", &["装"]);
        set_details(&app, &["dsh-himarket：0.1.7 → 0.1.8".to_string()]);
        fail_op(&app, "boom");
        start_op(&app, "launch", "启动 Harness", &[]);
        finish_op(&app, "ok");

        let state = PersistedState { current: current(), history: history(MAX_HISTORY) };
        let json = serde_json::to_string_pretty(&state).unwrap();
        let back: PersistedState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.history.len(), 1);
        assert_eq!(back.history[0].details.len(), 1);
        assert_eq!(back.history[0].state, OpState::Failed);
        assert_eq!(back.current.unwrap().id, "launch");
    }

    /// 旧格式（单 Operation 对象）必须仍能读入，不能因升级丢状态。
    ///
    /// ⚠️ 这是本轮**真实抓到的 bug**：若靠「反序列化失败就回退」，
    /// 旧格式会因 serde 的 default/未知字段容忍而「成功」解析成空状态，
    /// 导致用户上次的操作结果被静默丢弃。parse_state 显式按字段判别修复了它。
    #[test]
    fn legacy_single_object_still_loads() {
        let _guard = serial_lock();
        let legacy = r#"{"id":"launch","label":"启动 Harness","state":"done",
            "current_step":"完成","steps":[],"log":[],"result":"ok"}"#;
        let (cur, hist) = parse_state(legacy).expect("旧格式必须能解析");
        let op = cur.expect("旧格式应恢复出 current");
        assert_eq!(op.id, "launch");
        assert_eq!(op.state, OpState::Done);
        assert!(hist.is_empty());
        // 新字段缺省可反序列化（serde default）
        assert!(op.details.is_empty());
        assert!(op.started_at.is_empty());
    }

    /// 新格式往返：current + history 都恢复。
    #[test]
    fn new_format_roundtrip() {
        let _guard = serial_lock();
        let app = fake_app();
        reset_for_test();
        start_op(&app, "plugin-install", "安装插件", &["装"]);
        fail_op(&app, "err");
        start_op(&app, "launch", "启动 Harness", &[]);

        let json = serde_json::to_string_pretty(&PersistedState {
            current: current(),
            history: history(MAX_HISTORY),
        })
        .unwrap();
        let (cur, hist) = parse_state(&json).expect("新格式应能解析");
        assert_eq!(cur.unwrap().id, "launch");
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].id, "plugin-install");
    }

    /// 无法解析的内容不能 panic，返回 None。
    #[test]
    fn garbage_state_returns_none() {
        assert!(parse_state("not json at all").is_none());
        assert!(parse_state("").is_none());
    }
}
