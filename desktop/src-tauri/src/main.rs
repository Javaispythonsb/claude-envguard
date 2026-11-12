#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewWindow, WindowEvent};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const ALLOWED_CLIENTS: &[&str] = &[
    "clash-verge",
    "flclash",
    "sing-box",
    "clashx",
    "surge",
    "wireguard",
    "other",
    "auto",
];

struct OpGuard {
    busy: Arc<AtomicBool>,
    quitting: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum DockEdge {
    None,
    Top,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorkRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl WorkRect {
    fn right(self) -> i32 {
        self.x + self.w
    }
    fn bottom(self) -> i32 {
        self.y + self.h
    }
    fn contains_point(self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SavedDock {
    x: i32,
    y: i32,
    edge: DockEdge,
}

#[derive(Clone, Debug, Serialize)]
struct DockStatePayload {
    edge: DockEdge,
    tucked: bool,
    seam: bool,
    idle: bool,
    pinned: bool,
    preview: DockEdge,
}

struct DockInner {
    edge: DockEdge,
    dock_x: i32,
    dock_y: i32,
    work_area: WorkRect,
    tucked: bool,
    seam_mode: bool,
    idle: bool,
    pinned: bool,
    ui_hold: bool,
    preview: DockEdge,
    pending_user_move: bool,
    animating: bool,
    force_tuck: bool,
    force_untuck: bool,
    last_programmatic_target: Option<(i32, i32)>,
    last_move_at: Instant,
    last_hover_at: Instant,
    grace_until: Instant,
    ignore_moves_until: Instant,
}

impl DockInner {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            edge: DockEdge::Top,
            dock_x: 0,
            dock_y: 0,
            work_area: WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            tucked: false,
            seam_mode: false,
            idle: false,
            pinned: false,
            ui_hold: false,
            preview: DockEdge::None,
            pending_user_move: false,
            animating: false,
            force_tuck: false,
            force_untuck: false,
            last_programmatic_target: None,
            last_move_at: now,
            last_hover_at: now,
            grace_until: now + Duration::from_millis(2600),
            ignore_moves_until: now,
        }
    }

    fn payload(&self) -> DockStatePayload {
        DockStatePayload {
            edge: self.edge,
            tucked: self.tucked,
            seam: self.seam_mode,
            idle: self.idle,
            pinned: self.pinned,
            preview: self.preview,
        }
    }
}

struct DockController {
    inner: Arc<Mutex<DockInner>>,
}

struct BusyLease {
    flag: Arc<AtomicBool>,
}

impl BusyLease {
    fn try_acquire(flag: &Arc<AtomicBool>) -> Result<Self, String> {
        if flag
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("当前已有审计或维护操作正在进行，请稍候".to_string());
        }
        Ok(Self {
            flag: Arc::clone(flag),
        })
    }
}

impl Drop for BusyLease {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

#[derive(Serialize)]
struct CommandOutcome {
    output: String,
    exit_code: i32,
}

fn validate_launch_target(target: &str) -> Result<&'static str, String> {
    match target.trim() {
        "desktop" => Ok("desktop"),
        "cli" => Ok("cli"),
        _ => Err("启动目标仅允许 desktop 或 cli".to_string()),
    }
}

fn validate_client_mode(mode: &str) -> Result<String, String> {
    let trimmed = mode.trim();
    if ALLOWED_CLIENTS.contains(&trimmed) {
        Ok(trimmed.to_string())
    } else {
        Err("不支持的代理客户端模式".to_string())
    }
}

fn validate_flclash_port(port: &str) -> Result<String, String> {
    let raw = port.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("auto") {
        return Ok("auto".to_string());
    }
    if raw.len() > 64 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("端口格式无效".to_string());
    }
    if let Ok(num) = raw.parse::<u16>() {
        if num >= 1 {
            return Ok(num.to_string());
        }
    }
    for prefix in ["127.0.0.1:", "localhost:", "[::1]:"] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            if let Ok(num) = rest.parse::<u16>() {
                if num >= 1 {
                    return Ok(format!("127.0.0.1:{num}"));
                }
            }
        }
    }
    Err("只接受本机端口（如 9090 或 127.0.0.1:9090）或 auto".to_string())
}

fn validate_safe_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.len() > 2048 {
        return Err("链接为空或过长".to_string());
    }
    if trimmed
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '"')
    {
        return Err("链接包含非法空白或引号字符".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err("仅允许 https:// 或 http:// 链接".to_string());
    }
    Ok(trimmed.to_string())
}

fn validate_check_config_json(json_str: &str) -> Result<String, String> {
    if json_str.trim().is_empty() || json_str.len() > 8192 {
        return Err("检查策略配置为空或过大".to_string());
    }
    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|_| "检查策略 JSON 格式无效".to_string())?;
    if !parsed.is_object() {
        return Err("检查策略必须是 JSON 对象".to_string());
    }
    serde_json::to_string(&parsed).map_err(|_| "检查策略序列化失败".to_string())
}

// Release binaries must never run a script picked by an environment variable or
// by the build machine's workspace path, so these lookups exist only in debug builds.
#[cfg(debug_assertions)]
fn dev_core_script() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("ENVGUARD_CORE_SCRIPT") {
        let p = PathBuf::from(override_path);
        if p.is_file() {
            return Some(p);
        }
    }

    // Development workspace lookup (../windows/claude-switch.ps1 relative to desktop/)
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let dev_script = manifest_dir
        .parent()?
        .parent()?
        .join("windows")
        .join("claude-switch.ps1");
    dev_script.is_file().then_some(dev_script)
}

fn resolve_core_script(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    {
        if let Some(dev_script) = dev_core_script() {
            return Ok(dev_script);
        }
    }

    // Bundled Tauri resource lookup
    if let Ok(res_dir) = app.path().resource_dir() {
        for candidate in [
            res_dir.join("resources").join("claude-switch.ps1"),
            res_dir.join("claude-switch.ps1"),
        ] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    // Installed user profile fallback
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let installed = PathBuf::from(profile)
            .join(".claude-switcher")
            .join("claude-switch.ps1");
        if installed.is_file() {
            return Ok(installed);
        }
    }

    Err("未找到核心安全脚本 claude-switch.ps1".to_string())
}

fn execute_core_blocking(
    script: PathBuf,
    args: Vec<String>,
    stdin_payload: Option<String>,
    app_version: String,
) -> Result<CommandOutcome, String> {
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .args(&args)
        .env("ENVGUARD_APP_VERSION", app_version)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if stdin_payload.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 PowerShell 核心引擎: {e}"))?;

    if let Some(payload) = stdin_payload {
        if let Some(mut child_in) = child.stdin.take() {
            child_in
                .write_all(payload.as_bytes())
                .map_err(|e| format!("写入配置输入流失败: {e}"))?;
        }
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("等待核心引擎返回失败: {e}"))?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = if stderr.trim().is_empty() {
        stdout.trim().to_string()
    } else if stdout.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        format!("{}\n{}", stdout.trim(), stderr.trim())
    };

    Ok(CommandOutcome {
        output: combined,
        exit_code: out.status.code().unwrap_or(1),
    })
}

async fn run_core_async(app: &AppHandle, args: Vec<String>) -> Result<CommandOutcome, String> {
    run_core_with_stdin_async(app, args, None).await
}

async fn run_core_with_stdin_async(
    app: &AppHandle,
    args: Vec<String>,
    stdin_payload: Option<String>,
) -> Result<CommandOutcome, String> {
    let script = resolve_core_script(app)?;
    let app_version = app.package_info().version.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        execute_core_blocking(script, args, stdin_payload, app_version)
    })
    .await
    .map_err(|e| format!("后台任务执行异常: {e}"))?
}

// UTC timestamp file name; never derived from caller input.
fn diagnostics_file_name(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "envguard-diagnostics-{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z.zip",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

// Days since 1970-01-01 to (year, month, day), Howard Hinnant's civil_from_days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn tool_args(tool: &str, arg: Option<String>) -> Result<Vec<String>, String> {
    match tool.trim() {
        "set-region" => Ok(vec!["set-region".to_string()]),
        "rules-status" => Ok(vec!["rules-status".to_string()]),
        "fingerprint" => Ok(vec!["fingerprint".to_string()]),
        "upstream-status" => Ok(vec!["upstream-status".to_string()]),
        "open-safe" => {
            let raw_url = arg.ok_or_else(|| "缺少要隔离打开的链接".to_string())?;
            let valid_url = validate_safe_url(&raw_url)?;
            Ok(vec!["open-safe".to_string(), valid_url])
        }
        _ => Err("拒绝执行未授权的工具命令".to_string()),
    }
}

#[tauri::command]
async fn get_ui_state(
    app: AppHandle,
    guard: State<'_, OpGuard>,
    fresh: bool,
) -> Result<serde_json::Value, String> {
    let _lease = if fresh {
        Some(BusyLease::try_acquire(&guard.busy)?)
    } else {
        None
    };

    let mut args = vec!["uistate".to_string()];
    if fresh {
        args.push("--fresh".to_string());
    }
    let res = run_core_async(&app, args).await?;
    let json_line = res
        .output
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| format!("状态解析失败: {}", res.output))?;

    serde_json::from_str(json_line).map_err(|e| format!("状态 JSON 无效: {e}"))
}

#[tauri::command]
async fn safe_launch(
    app: AppHandle,
    guard: State<'_, OpGuard>,
    target: String,
) -> Result<CommandOutcome, String> {
    let _lease = BusyLease::try_acquire(&guard.busy)?;
    let validated = validate_launch_target(&target)?;
    run_core_async(&app, vec!["launch".to_string(), validated.to_string()]).await
}

#[tauri::command]
async fn run_fix(app: AppHandle, guard: State<'_, OpGuard>) -> Result<CommandOutcome, String> {
    let _lease = BusyLease::try_acquire(&guard.busy)?;
    run_core_async(&app, vec!["fix".to_string()]).await
}

#[tauri::command]
async fn shield_restore(
    app: AppHandle,
    guard: State<'_, OpGuard>,
) -> Result<CommandOutcome, String> {
    let _lease = BusyLease::try_acquire(&guard.busy)?;
    run_core_async(&app, vec!["shield-restore".to_string()]).await
}

#[tauri::command]
async fn set_client_mode(
    app: AppHandle,
    guard: State<'_, OpGuard>,
    mode: String,
) -> Result<CommandOutcome, String> {
    let _lease = BusyLease::try_acquire(&guard.busy)?;
    let valid_mode = validate_client_mode(&mode)?;
    run_core_async(&app, vec!["set-client".to_string(), valid_mode]).await
}

#[tauri::command]
async fn set_flclash_port(
    app: AppHandle,
    guard: State<'_, OpGuard>,
    port: String,
) -> Result<CommandOutcome, String> {
    let _lease = BusyLease::try_acquire(&guard.busy)?;
    let valid_port = validate_flclash_port(&port)?;
    run_core_async(
        &app,
        vec!["set-flclash-controller".to_string(), valid_port],
    )
    .await
}

#[tauri::command]
async fn save_check_config(
    app: AppHandle,
    guard: State<'_, OpGuard>,
    config_json: String,
) -> Result<CommandOutcome, String> {
    let _lease = BusyLease::try_acquire(&guard.busy)?;
    let clean_json = validate_check_config_json(&config_json)?;
    run_core_with_stdin_async(
        &app,
        vec!["set-check-config".to_string(), "--stdin".to_string()],
        Some(clean_json),
    )
    .await
}

#[tauri::command]
async fn run_tool(
    app: AppHandle,
    guard: State<'_, OpGuard>,
    tool: String,
    arg: Option<String>,
) -> Result<CommandOutcome, String> {
    let _lease = BusyLease::try_acquire(&guard.busy)?;
    let args = tool_args(&tool, arg)?;
    run_core_async(&app, args).await
}

#[tauri::command]
async fn export_diagnostics(
    app: AppHandle,
    guard: State<'_, OpGuard>,
) -> Result<CommandOutcome, String> {
    let _lease = BusyLease::try_acquire(&guard.busy)?;
    let dir = app
        .path()
        .desktop_dir()
        .or_else(|_| app.path().download_dir())
        .map_err(|_| "无法定位桌面或下载目录".to_string())?;
    let target = dir.join(diagnostics_file_name(SystemTime::now()));
    let mut outcome = run_core_async(
        &app,
        vec![
            "export-diagnostics".to_string(),
            target.to_string_lossy().into_owned(),
        ],
    )
    .await?;
    if outcome.exit_code == 0 {
        outcome.output = format!("{}\n已保存到: {}", outcome.output, target.display());
    }
    Ok(outcome)
}

#[cfg(target_os = "windows")]
fn is_primary_mouse_down() -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(vkey: i32) -> i16;
        fn GetSystemMetrics(index: i32) -> i32;
    }
    const VK_LBUTTON: i32 = 0x01;
    const VK_RBUTTON: i32 = 0x02;
    const SM_SWAPBUTTON: i32 = 23;
    // SAFETY: Stateless Win32 queries safe to call from any thread.
    unsafe {
        let vk = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        (GetAsyncKeyState(vk) as u16 & 0x8000) != 0
    }
}

#[cfg(not(target_os = "windows"))]
fn is_primary_mouse_down() -> bool {
    false
}

const PEEK_LOGICAL_PX: f64 = 10.0;
const SNAP_THRESHOLD_LOGICAL_PX: f64 = 48.0;

fn detect_snap_edge(wx: i32, wy: i32, ww: i32, _wh: i32, area: WorkRect, threshold: i32) -> DockEdge {
    let d_top = wy - area.y;
    let d_left = wx - area.x;
    let d_right = area.right() - (wx + ww);

    let candidates = [
        (d_top, DockEdge::Top),
        (d_left, DockEdge::Left),
        (d_right, DockEdge::Right),
    ];
    if let Some(&(min_d, edge)) = candidates.iter().min_by_key(|c| c.0) {
        if min_d <= threshold {
            return edge;
        }
    }
    DockEdge::None
}

fn snap_window_rect(
    wx: i32,
    wy: i32,
    ww: i32,
    wh: i32,
    area: WorkRect,
    threshold: i32,
    margin: i32,
) -> (i32, i32, DockEdge) {
    let edge = detect_snap_edge(wx, wy, ww, wh, area, threshold);
    let min_x = area.x + margin;
    let max_x = (area.right() - ww - margin).max(min_x);
    let min_y = area.y + margin;
    let max_y = (area.bottom() - wh - margin).max(min_y);

    match edge {
        DockEdge::Top => (wx.clamp(min_x, max_x), area.y, DockEdge::Top),
        DockEdge::Left => (area.x, wy.clamp(min_y, max_y), DockEdge::Left),
        DockEdge::Right => (area.right() - ww, wy.clamp(min_y, max_y), DockEdge::Right),
        DockEdge::None => (wx.clamp(min_x, max_x), wy.clamp(min_y, max_y), DockEdge::None),
    }
}

fn tucked_window_pos(
    dock_x: i32,
    dock_y: i32,
    ww: i32,
    wh: i32,
    area: WorkRect,
    peek: i32,
    edge: DockEdge,
    seam: bool,
) -> (i32, i32) {
    if seam {
        return (dock_x, dock_y);
    }
    match edge {
        DockEdge::Top => (dock_x, area.y - wh + peek),
        DockEdge::Left => (area.x - ww + peek, dock_y),
        DockEdge::Right => (area.right() - peek, dock_y),
        DockEdge::None => (dock_x, dock_y),
    }
}

fn is_cursor_in_tucked_handle(
    cx: i32,
    cy: i32,
    dock_x: i32,
    dock_y: i32,
    ww: i32,
    wh: i32,
    area: WorkRect,
    scale: f64,
    edge: DockEdge,
) -> bool {
    let depth = (14.0 * scale).round() as i32;
    match edge {
        DockEdge::Top => {
            let center_x = dock_x + ww / 2;
            let half_w = (84.0 * scale).round() as i32;
            cx >= center_x - half_w
                && cx <= center_x + half_w
                && cy >= area.y - 4
                && cy <= area.y + depth
        }
        DockEdge::Left => {
            let center_y = dock_y + wh / 2;
            let half_h = (54.0 * scale).round() as i32;
            cx >= area.x - 4
                && cx <= area.x + depth
                && cy >= center_y - half_h
                && cy <= center_y + half_h
        }
        DockEdge::Right => {
            let center_y = dock_y + wh / 2;
            let half_h = (54.0 * scale).round() as i32;
            cx >= area.right() - depth
                && cx <= area.right() + 4
                && cy >= center_y - half_h
                && cy <= center_y + half_h
        }
        DockEdge::None => false,
    }
}

fn is_cursor_in_pod(cx: i32, cy: i32, wx: i32, wy: i32, ww: i32, wh: i32, pad: i32) -> bool {
    cx >= wx - pad && cx <= wx + ww + pad && cy >= wy - pad && cy <= wy + wh + pad
}

fn has_adjacent_monitor_beyond_edge(
    window: &WebviewWindow,
    dock_x: i32,
    dock_y: i32,
    ww: i32,
    wh: i32,
    area: WorkRect,
    edge: DockEdge,
) -> bool {
    let probe = match edge {
        DockEdge::Top => (dock_x + ww / 2, area.y - 24),
        DockEdge::Left => (area.x - 24, dock_y + wh / 2),
        DockEdge::Right => (area.right() + 24, dock_y + wh / 2),
        DockEdge::None => return false,
    };
    let Ok(monitors) = window.available_monitors() else {
        return false;
    };
    monitors.iter().any(|m| {
        let p = m.position();
        let s = m.size();
        let rect = WorkRect {
            x: p.x,
            y: p.y,
            w: s.width as i32,
            h: s.height as i32,
        };
        rect.contains_point(probe.0, probe.1)
    })
}

fn dock_state_file_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ENVGUARD_STATE_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("window-dock.json"));
        }
    }
    let profile = std::env::var("USERPROFILE").ok()?;
    Some(
        PathBuf::from(profile)
            .join(".claude-switcher")
            .join("window-dock.json"),
    )
}

fn load_saved_dock() -> Option<SavedDock> {
    let path = dock_state_file_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn persist_saved_dock(saved: &SavedDock) {
    if let Some(path) = dock_state_file_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(saved) {
            let _ = std::fs::write(path, json);
        }
    }
}

fn monitor_work_rect(window: &WebviewWindow, ref_x: i32, ref_y: i32) -> Option<WorkRect> {
    let mon = window
        .monitor_from_point(ref_x as f64, ref_y as f64)
        .ok()
        .flatten()
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten())?;
    let wa = mon.work_area();
    Some(WorkRect {
        x: wa.position.x,
        y: wa.position.y,
        w: wa.size.width as i32,
        h: wa.size.height as i32,
    })
}

fn set_programmatic_pos(window: &WebviewWindow, dock: &Arc<Mutex<DockInner>>, x: i32, y: i32) {
    {
        let mut st = dock.lock().unwrap();
        st.last_programmatic_target = Some((x, y));
        st.ignore_moves_until = Instant::now() + Duration::from_millis(280);
    }
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn glide_window(
    window: &WebviewWindow,
    dock: &Arc<Mutex<DockInner>>,
    from: (i32, i32),
    to: (i32, i32),
    steps: u32,
    step_ms: u64,
) {
    if from == to {
        return;
    }
    {
        let mut st = dock.lock().unwrap();
        st.animating = true;
    }
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let eased = 1.0 - (1.0 - t).powi(3);
        let x = (from.0 as f64 + (to.0 - from.0) as f64 * eased).round() as i32;
        let y = (from.1 as f64 + (to.1 - from.1) as f64 * eased).round() as i32;
        set_programmatic_pos(window, dock, x, y);
        if i < steps {
            std::thread::sleep(Duration::from_millis(step_ms));
        }
    }
    let mut st = dock.lock().unwrap();
    st.animating = false;
    st.last_programmatic_target = Some(to);
    st.ignore_moves_until = Instant::now() + Duration::from_millis(280);
}

fn place_top_center(window: &WebviewWindow, dock: &Arc<Mutex<DockInner>>, persist: bool) {
    let size = window
        .outer_size()
        .unwrap_or(tauri::PhysicalSize::new(540, 128));
    let ww = size.width as i32;
    let wh = size.height as i32;
    let mon = window
        .primary_monitor()
        .ok()
        .flatten()
        .or_else(|| window.current_monitor().ok().flatten());
    if let Some(m) = mon {
        let wa = m.work_area();
        let area = WorkRect {
            x: wa.position.x,
            y: wa.position.y,
            w: wa.size.width as i32,
            h: wa.size.height as i32,
        };
        let x = area.x + ((area.w - ww) / 2).max(0);
        let y = area.y;
        let seam = has_adjacent_monitor_beyond_edge(window, x, y, ww, wh, area, DockEdge::Top);
        let now = Instant::now();
        let payload = {
            let mut st = dock.lock().unwrap();
            st.edge = DockEdge::Top;
            st.dock_x = x;
            st.dock_y = y;
            st.work_area = area;
            st.seam_mode = seam;
            st.tucked = false;
            st.idle = false;
            st.preview = DockEdge::None;
            st.pending_user_move = false;
            st.force_tuck = false;
            st.force_untuck = false;
            st.last_hover_at = now;
            st.grace_until = now + Duration::from_millis(2800);
            st.payload()
        };
        let _ = window.set_ignore_cursor_events(false);
        set_programmatic_pos(window, dock, x, y);
        if persist {
            persist_saved_dock(&SavedDock {
                x,
                y,
                edge: DockEdge::Top,
            });
        }
        let _ = window.emit("dock-state", payload);
    }
}

fn init_window_dock(window: &WebviewWindow, dock: &Arc<Mutex<DockInner>>) {
    let _ = window.set_always_on_top(true);
    let size = window
        .outer_size()
        .unwrap_or(tauri::PhysicalSize::new(540, 128));
    let ww = size.width as i32;
    let wh = size.height as i32;
    let scale = window.scale_factor().unwrap_or(1.0);
    let margin = (8.0 * scale).round() as i32;

    if let Some(saved) = load_saved_dock() {
        if let Some(area) = monitor_work_rect(window, saved.x + ww / 2, saved.y + wh / 2) {
            if area.contains_point(saved.x + ww / 2, saved.y + 24) {
                let min_x = area.x + margin;
                let max_x = (area.right() - ww - margin).max(min_x);
                let min_y = area.y + margin;
                let max_y = (area.bottom() - wh - margin).max(min_y);
                let (x, y) = match saved.edge {
                    DockEdge::Top => (saved.x.clamp(min_x, max_x), area.y),
                    DockEdge::Left => (area.x, saved.y.clamp(min_y, max_y)),
                    DockEdge::Right => (area.right() - ww, saved.y.clamp(min_y, max_y)),
                    DockEdge::None => (saved.x.clamp(min_x, max_x), saved.y.clamp(min_y, max_y)),
                };
                let seam =
                    has_adjacent_monitor_beyond_edge(window, x, y, ww, wh, area, saved.edge);
                let now = Instant::now();
                {
                    let mut st = dock.lock().unwrap();
                    st.edge = saved.edge;
                    st.dock_x = x;
                    st.dock_y = y;
                    st.work_area = area;
                    st.seam_mode = seam;
                    st.tucked = false;
                    st.idle = false;
                    st.last_hover_at = now;
                    st.grace_until = now + Duration::from_millis(2600);
                }
                let _ = window.set_ignore_cursor_events(false);
                set_programmatic_pos(window, dock, x, y);
                return;
            }
        }
    }

    place_top_center(window, dock, false);
}

enum WatcherAction {
    None,
    Snap {
        to: (i32, i32),
        saved: SavedDock,
        payload: DockStatePayload,
    },
    AdjustOnScreen {
        to: (i32, i32),
    },
    Tuck {
        from: (i32, i32),
        edge: DockEdge,
        dock_x: i32,
        dock_y: i32,
        area: WorkRect,
        seam: bool,
        peek: i32,
    },
    Untuck {
        from: (i32, i32),
        to: (i32, i32),
        payload: DockStatePayload,
    },
    EmitOnly(DockStatePayload),
}

fn start_dock_watcher(
    window: WebviewWindow,
    dock: Arc<Mutex<DockInner>>,
    quitting: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        while !quitting.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(35));
            if quitting.load(Ordering::SeqCst) {
                break;
            }
            if !window.is_visible().unwrap_or(false) {
                continue;
            }
            let (Ok(cur), Ok(wpos), Ok(wsize)) = (
                window.cursor_position(),
                window.outer_position(),
                window.outer_size(),
            ) else {
                continue;
            };
            let cx = cur.x.round() as i32;
            let cy = cur.y.round() as i32;
            let wx = wpos.x;
            let wy = wpos.y;
            let ww = wsize.width as i32;
            let wh = wsize.height as i32;
            let scale = window.scale_factor().unwrap_or(1.0);
            let peek = (PEEK_LOGICAL_PX * scale).round() as i32;
            let threshold = (SNAP_THRESHOLD_LOGICAL_PX * scale).round() as i32;
            let margin = (8.0 * scale).round() as i32;
            let mouse_down = is_primary_mouse_down();
            let now = Instant::now();

            let mut action = WatcherAction::None;

            {
                let mut st = dock.lock().unwrap();
                if st.animating {
                    continue;
                }

                if st.force_untuck && st.tucked {
                    st.force_untuck = false;
                    st.force_tuck = false;
                    st.tucked = false;
                    st.idle = false;
                    st.last_hover_at = now;
                    st.grace_until = now + Duration::from_millis(2400);
                    let to = (st.dock_x, st.dock_y);
                    action = WatcherAction::Untuck {
                        from: (wx, wy),
                        to,
                        payload: st.payload(),
                    };
                } else if st.force_tuck && !st.tucked && st.edge != DockEdge::None {
                    st.force_tuck = false;
                    st.pinned = false;
                    st.ui_hold = false;
                    action = WatcherAction::Tuck {
                        from: (wx, wy),
                        edge: st.edge,
                        dock_x: st.dock_x,
                        dock_y: st.dock_y,
                        area: st.work_area,
                        seam: st.seam_mode,
                        peek,
                    };
                } else if st.pending_user_move {
                    let live_area = monitor_work_rect(&window, wx + ww / 2, wy + (wh.min(160)) / 2)
                        .unwrap_or(st.work_area);
                    if mouse_down {
                        let prev = detect_snap_edge(wx, wy, ww, wh, live_area, threshold);
                        if prev != st.preview {
                            st.preview = prev;
                            action = WatcherAction::EmitOnly(st.payload());
                        }
                    } else {
                        st.pending_user_move = false;
                        st.preview = DockEdge::None;
                        let (sx, sy, new_edge) =
                            snap_window_rect(wx, wy, ww, wh, live_area, threshold, margin);
                        let seam = has_adjacent_monitor_beyond_edge(
                            &window, sx, sy, ww, wh, live_area, new_edge,
                        );
                        st.edge = new_edge;
                        st.dock_x = sx;
                        st.dock_y = sy;
                        st.work_area = live_area;
                        st.seam_mode = seam;
                        st.tucked = false;
                        st.idle = false;
                        st.last_hover_at = now;
                        st.grace_until = now + Duration::from_millis(1400);
                        action = WatcherAction::Snap {
                            to: (sx, sy),
                            saved: SavedDock {
                                x: sx,
                                y: sy,
                                edge: new_edge,
                            },
                            payload: st.payload(),
                        };
                    }
                } else {
                    match st.edge {
                        DockEdge::Top | DockEdge::Left | DockEdge::Right => {
                            if st.tucked {
                                if !mouse_down
                                    && is_cursor_in_tucked_handle(
                                        cx,
                                        cy,
                                        st.dock_x,
                                        st.dock_y,
                                        ww,
                                        wh,
                                        st.work_area,
                                        scale,
                                        st.edge,
                                    )
                                {
                                    st.tucked = false;
                                    st.idle = false;
                                    st.last_hover_at = now;
                                    st.grace_until = now + Duration::from_millis(900);
                                    let to = (st.dock_x, st.dock_y);
                                    action = WatcherAction::Untuck {
                                        from: (wx, wy),
                                        to,
                                        payload: st.payload(),
                                    };
                                }
                            } else {
                                // Keep docked edge flush and prevent tall drawers from overflowing taskbar
                                let area = st.work_area;
                                let mut target_x = wx;
                                let mut target_y = wy;
                                if st.edge == DockEdge::Right && (wx + ww - area.right()).abs() > 2
                                {
                                    target_x = area.right() - ww;
                                    st.dock_x = target_x;
                                } else if st.edge == DockEdge::Left && (wx - area.x).abs() > 2 {
                                    target_x = area.x;
                                    st.dock_x = target_x;
                                } else if st.edge == DockEdge::Top && (wy - area.y).abs() > 2 {
                                    target_y = area.y;
                                    st.dock_y = target_y;
                                }
                                if st.edge != DockEdge::Top && wy + wh > area.bottom() - margin {
                                    target_y = (area.bottom() - wh - margin).max(area.y);
                                }
                                if (target_x, target_y) != (wx, wy)
                                    && now >= st.ignore_moves_until
                                {
                                    action = WatcherAction::AdjustOnScreen {
                                        to: (target_x, target_y),
                                    };
                                } else {
                                    let pad = (8.0 * scale).round() as i32;
                                    let inside = is_cursor_in_pod(cx, cy, wx, wy, ww, wh, pad);
                                    if inside || st.pinned || st.ui_hold || mouse_down {
                                        st.last_hover_at = now;
                                    } else if now >= st.grace_until
                                        && now.duration_since(st.last_hover_at)
                                            >= Duration::from_millis(1200)
                                    {
                                        action = WatcherAction::Tuck {
                                            from: (wx, wy),
                                            edge: st.edge,
                                            dock_x: st.dock_x,
                                            dock_y: st.dock_y,
                                            area: st.work_area,
                                            seam: st.seam_mode,
                                            peek,
                                        };
                                    }
                                }
                            }
                        }
                        DockEdge::None => {
                            let pad = (6.0 * scale).round() as i32;
                            let inside = is_cursor_in_pod(cx, cy, wx, wy, ww, wh, pad);
                            if inside || st.pinned || st.ui_hold || mouse_down {
                                st.last_hover_at = now;
                                if st.idle {
                                    st.idle = false;
                                    action = WatcherAction::EmitOnly(st.payload());
                                }
                            } else if !st.idle
                                && now >= st.grace_until
                                && now.duration_since(st.last_hover_at)
                                    >= Duration::from_millis(2200)
                            {
                                st.idle = true;
                                action = WatcherAction::EmitOnly(st.payload());
                            }
                        }
                    }
                }
            }

            match action {
                WatcherAction::None => {}
                WatcherAction::EmitOnly(payload) => {
                    let _ = window.emit("dock-state", payload);
                }
                WatcherAction::AdjustOnScreen { to } => {
                    set_programmatic_pos(&window, &dock, to.0, to.1);
                }
                WatcherAction::Snap { to, saved, payload } => {
                    let _ = window.set_ignore_cursor_events(false);
                    if let Ok(cur_pos) = window.outer_position() {
                        if (cur_pos.x, cur_pos.y) != to {
                            glide_window(&window, &dock, (cur_pos.x, cur_pos.y), to, 6, 14);
                        }
                    }
                    persist_saved_dock(&saved);
                    let _ = window.emit("dock-state", payload);
                }
                WatcherAction::Tuck {
                    from,
                    edge,
                    dock_x,
                    dock_y,
                    area,
                    seam,
                    peek,
                } => {
                    // Collapse any open drawer first so collapsed capsule dimensions are cleanly used
                    let _ = window.emit("collapse-drawers", ());
                    std::thread::sleep(Duration::from_millis(45));
                    let fresh_size = window.outer_size().unwrap_or(wsize);
                    let f_ww = fresh_size.width as i32;
                    let f_wh = fresh_size.height as i32;
                    let cur_from = window
                        .outer_position()
                        .map(|p| (p.x, p.y))
                        .unwrap_or(from);
                    let target =
                        tucked_window_pos(dock_x, dock_y, f_ww, f_wh, area, peek, edge, seam);
                    glide_window(&window, &dock, cur_from, target, 10, 15);
                    let payload = {
                        let mut st = dock.lock().unwrap();
                        st.tucked = true;
                        st.idle = true;
                        st.payload()
                    };
                    let _ = window.set_ignore_cursor_events(true);
                    let _ = window.emit("dock-state", payload);
                }
                WatcherAction::Untuck { from, to, payload } => {
                    let _ = window.set_ignore_cursor_events(false);
                    let _ = window.emit("dock-state", payload);
                    glide_window(&window, &dock, from, to, 10, 14);
                }
            }
        }
    });
}

#[tauri::command]
fn get_dock_state(dock: State<'_, DockController>) -> DockStatePayload {
    dock.inner.lock().unwrap().payload()
}

#[tauri::command]
fn set_dock_hold(dock: State<'_, DockController>, hold: bool) {
    let mut st = dock.inner.lock().unwrap();
    st.ui_hold = hold;
    if hold {
        st.last_hover_at = Instant::now();
    }
}

#[tauri::command]
fn set_dock_pinned(app: AppHandle, dock: State<'_, DockController>, pinned: bool) -> DockStatePayload {
    let (was_tucked, payload) = {
        let mut st = dock.inner.lock().unwrap();
        let prev_tucked = st.tucked;
        st.pinned = pinned;
        st.last_hover_at = Instant::now();
        if pinned && prev_tucked {
            st.force_untuck = true;
        } else if pinned {
            st.idle = false;
        }
        (prev_tucked, st.payload())
    };
    if !was_tucked {
        if let Some(window) = app.get_webview_window("main") {
            if pinned {
                let _ = window.set_ignore_cursor_events(false);
            }
            let _ = window.emit("dock-state", payload.clone());
        }
    }
    payload
}

#[tauri::command]
fn tuck_or_hide(app: AppHandle, dock: State<'_, DockController>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let should_hide = {
        let mut st = dock.inner.lock().unwrap();
        if st.edge != DockEdge::None {
            st.force_tuck = true;
            false
        } else {
            true
        }
    };
    if should_hide {
        let _ = window.hide();
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let mut immediate_payload: Option<DockStatePayload> = None;
        {
            let dock = app.state::<DockController>();
            let mut st = dock.inner.lock().unwrap();
            let now = Instant::now();
            st.last_hover_at = now;
            st.grace_until = now + Duration::from_millis(3200);
            if st.tucked {
                st.force_untuck = true;
            } else {
                st.idle = false;
                immediate_payload = Some(st.payload());
            }
        }
        let _ = window.set_ignore_cursor_events(false);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        if let Some(payload) = immediate_payload {
            let _ = window.emit("dock-state", payload);
        }
    }
}

fn map_tray_menu_event(id: &str) -> Option<&'static str> {
    match id {
        "launch_desktop" => Some("launch-desktop"),
        "launch_cli" => Some("launch-cli"),
        "recheck" => Some("recheck"),
        "shield_restore" => Some("shield-restore"),
        _ => None,
    }
}

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let open_console = MenuItemBuilder::with_id("open_console", "浮现悬浮窗").build(app)?;
    let reset_top =
        MenuItemBuilder::with_id("reset_top", "贴靠回屏幕顶部居中").build(app)?;
    let launch_desktop =
        MenuItemBuilder::with_id("launch_desktop", "▶ 安全启动 Claude").build(app)?;
    let launch_cli = MenuItemBuilder::with_id("launch_cli", "❯_ 安全启动 CLI").build(app)?;
    let recheck = MenuItemBuilder::with_id("recheck", "↻ 重新检查环境").build(app)?;
    let shield_restore =
        MenuItemBuilder::with_id("shield_restore", "退出护盾（还原网络并停保镖）").build(app)?;
    let quit_app =
        MenuItemBuilder::with_id("quit_app", "退出悬浮窗（保留后台保镖）").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&open_console)
        .item(&reset_top)
        .separator()
        .item(&launch_desktop)
        .item(&launch_cli)
        .item(&recheck)
        .separator()
        .item(&shield_restore)
        .item(&quit_app)
        .build()?;

    let mut builder = TrayIconBuilder::with_id("envguard-tray")
        .tooltip("Claude 环境卫士 · EnvGuard")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            if id == "open_console" {
                show_main_window(app);
            } else if id == "reset_top" {
                show_main_window(app);
                if let Some(window) = app.get_webview_window("main") {
                    let dock = app.state::<DockController>();
                    place_top_center(&window, &dock.inner, true);
                }
            } else if id == "quit_app" {
                app.state::<OpGuard>()
                    .quitting
                    .store(true, Ordering::SeqCst);
                app.exit(0);
            } else if let Some(action) = map_tray_menu_event(id) {
                show_main_window(app);
                let _ = app.emit("tray-action", action);
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}

fn main() {
    let quitting = Arc::new(AtomicBool::new(false));
    let dock_inner = Arc::new(Mutex::new(DockInner::new()));

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .manage(OpGuard {
            busy: Arc::new(AtomicBool::new(false)),
            quitting: Arc::clone(&quitting),
        })
        .manage(DockController {
            inner: Arc::clone(&dock_inner),
        })
        .setup({
            let quitting = Arc::clone(&quitting);
            let dock_inner = Arc::clone(&dock_inner);
            move |app| {
                setup_tray(app.handle())?;
                if let Some(win) = app.get_webview_window("main") {
                    init_window_dock(&win, &dock_inner);
                    start_dock_watcher(win, dock_inner, quitting);
                }
                Ok(())
            }
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                match event {
                    WindowEvent::Moved(pos) => {
                        let dock = window.app_handle().state::<DockController>();
                        let now = Instant::now();
                        let mut untuck = false;
                        {
                            let mut st = dock.inner.lock().unwrap();
                            let is_prog = st.animating
                                || now < st.ignore_moves_until
                                || st.last_programmatic_target == Some((pos.x, pos.y));
                            if !is_prog {
                                st.pending_user_move = true;
                                st.last_move_at = now;
                                st.last_hover_at = now;
                                if st.tucked {
                                    st.tucked = false;
                                    untuck = true;
                                }
                            }
                        }
                        if untuck {
                            let _ = window.set_ignore_cursor_events(false);
                        }
                    }
                    WindowEvent::CloseRequested { api, .. } => {
                        let quitting = window
                            .app_handle()
                            .state::<OpGuard>()
                            .quitting
                            .load(Ordering::SeqCst);
                        if !quitting {
                            api.prevent_close();
                            let _ = window.hide();
                        }
                    }
                    _ => {}
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_ui_state,
            safe_launch,
            run_fix,
            shield_restore,
            set_client_mode,
            set_flclash_port,
            save_check_config,
            run_tool,
            export_diagnostics,
            get_dock_state,
            set_dock_hold,
            set_dock_pinned,
            tuck_or_hide
        ])
        .run(tauri::generate_context!())
        .expect("error while running Claude EnvGuard");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_target_whitelist() {
        assert_eq!(validate_launch_target("desktop").unwrap(), "desktop");
        assert_eq!(validate_launch_target("cli").unwrap(), "cli");
        assert!(validate_launch_target("desktop; calc.exe").is_err());
        assert!(validate_launch_target("switch").is_err());
    }

    #[test]
    fn client_mode_whitelist() {
        assert_eq!(validate_client_mode("clash-verge").unwrap(), "clash-verge");
        assert_eq!(validate_client_mode("flclash").unwrap(), "flclash");
        assert!(validate_client_mode("clash-verge\" ; whoami").is_err());
    }

    #[test]
    fn flclash_port_loopback_only() {
        assert_eq!(validate_flclash_port("").unwrap(), "auto");
        assert_eq!(validate_flclash_port("9090").unwrap(), "9090");
        assert_eq!(
            validate_flclash_port("127.0.0.1:9097").unwrap(),
            "127.0.0.1:9097"
        );
        assert!(validate_flclash_port("192.168.1.5:9090").is_err());
        assert!(validate_flclash_port("0").is_err());
    }

    #[test]
    fn safe_url_validation() {
        assert!(validate_safe_url("https://claude.ai/magic?token=abc").is_ok());
        assert!(validate_safe_url("javascript:alert(1)").is_err());
        assert!(validate_safe_url("file:///C:/Windows/win.ini").is_err());
        assert!(validate_safe_url("https://example.com/\" --evil").is_err());
    }

    #[test]
    fn diagnostics_file_name_is_utc_timestamp() {
        use std::time::Duration;
        assert_eq!(
            diagnostics_file_name(UNIX_EPOCH),
            "envguard-diagnostics-19700101T000000Z.zip"
        );
        assert_eq!(
            diagnostics_file_name(UNIX_EPOCH + Duration::from_secs(1_709_164_800)),
            "envguard-diagnostics-20240229T000000Z.zip"
        );
        assert_eq!(
            diagnostics_file_name(UNIX_EPOCH + Duration::from_secs(1_794_411_000)),
            "envguard-diagnostics-20261111T153000Z.zip"
        );
    }

    #[test]
    fn tool_whitelist() {
        assert_eq!(
            tool_args("upstream-status", None).unwrap(),
            vec!["upstream-status"]
        );
        assert_eq!(
            tool_args("open-safe", Some("https://claude.ai/".to_string())).unwrap(),
            vec!["open-safe", "https://claude.ai/"]
        );
        assert!(tool_args("open-safe", None).is_err());
        assert!(tool_args("update-now", None).is_err());
        assert!(tool_args("export-diagnostics", None).is_err());
    }

    #[test]
    fn busy_lease_blocks_concurrent_mutation() {
        let flag = Arc::new(AtomicBool::new(false));
        let l1 = BusyLease::try_acquire(&flag);
        assert!(l1.is_ok());
        assert!(BusyLease::try_acquire(&flag).is_err());
        drop(l1);
        assert!(BusyLease::try_acquire(&flag).is_ok());
    }

    #[test]
    fn tray_menu_action_mapping() {
        assert_eq!(map_tray_menu_event("launch_desktop"), Some("launch-desktop"));
        assert_eq!(map_tray_menu_event("launch_cli"), Some("launch-cli"));
        assert_eq!(map_tray_menu_event("recheck"), Some("recheck"));
        assert_eq!(map_tray_menu_event("shield_restore"), Some("shield-restore"));
        assert_eq!(map_tray_menu_event("open_console"), None);
        assert_eq!(map_tray_menu_event("quit_app"), None);
        assert_eq!(map_tray_menu_event("unknown"), None);
    }

    #[test]
    fn edge_snap_and_tuck_positions_for_all_three_edges() {
        let area = WorkRect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        };
        // Dragged near top -> snaps flush to Top (y = 0) and tucks upward leaving 10px peek
        assert_eq!(
            snap_window_rect(1010, 26, 540, 128, area, 48, 8),
            (1010, 0, DockEdge::Top)
        );
        assert_eq!(
            tucked_window_pos(1010, 0, 540, 128, area, 10, DockEdge::Top, false),
            (1010, -118)
        );

        // Dragged near right edge -> snaps flush to Right (x = 2560 - 540 = 2020) and tucks rightward to x = 2550
        assert_eq!(
            snap_window_rect(2005, 520, 540, 128, area, 48, 8),
            (2020, 520, DockEdge::Right)
        );
        assert_eq!(
            tucked_window_pos(2020, 520, 540, 128, area, 10, DockEdge::Right, false),
            (2550, 520)
        );

        // Dragged near left edge -> snaps flush to Left (x = 0) and tucks leftward to x = -530
        assert_eq!(
            snap_window_rect(18, 480, 540, 128, area, 48, 8),
            (0, 480, DockEdge::Left)
        );
        assert_eq!(
            tucked_window_pos(0, 480, 540, 128, area, 10, DockEdge::Left, false),
            (-530, 480)
        );

        // Dragged to center -> stays free-floating (DockEdge::None)
        assert_eq!(
            snap_window_rect(800, 450, 540, 128, area, 48, 8),
            (800, 450, DockEdge::None)
        );
    }

    #[test]
    fn edge_handle_trigger_zones_top_left_right() {
        let area = WorkRect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        };
        // Top docked at (1010, 0), width 540 -> center_x = 1280
        assert!(is_cursor_in_tucked_handle(
            1280, 2, 1010, 0, 540, 128, area, 1.0, DockEdge::Top
        ));
        assert!(!is_cursor_in_tucked_handle(
            2400, 2, 1010, 0, 540, 128, area, 1.0, DockEdge::Top
        ));
        assert!(!is_cursor_in_tucked_handle(
            1280, 35, 1010, 0, 540, 128, area, 1.0, DockEdge::Top
        ));

        // Left docked at (0, 500), height 128 -> center_y = 564, x in [-4..14]
        assert!(is_cursor_in_tucked_handle(
            3, 560, 0, 500, 540, 128, area, 1.0, DockEdge::Left
        ));
        assert!(!is_cursor_in_tucked_handle(
            3, 900, 0, 500, 540, 128, area, 1.0, DockEdge::Left
        ));

        // Right docked at (2020, 500), height 128 -> center_y = 564, x in [2546..2564]
        assert!(is_cursor_in_tucked_handle(
            2557, 560, 2020, 500, 540, 128, area, 1.0, DockEdge::Right
        ));
        assert!(!is_cursor_in_tucked_handle(
            2557, 200, 2020, 500, 540, 128, area, 1.0, DockEdge::Right
        ));
    }
}
