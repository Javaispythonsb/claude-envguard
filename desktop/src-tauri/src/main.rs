#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewWindow, WindowEvent,
};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StandbyMode {
    Edge,
    Orb,
}

impl Default for StandbyMode {
    fn default() -> Self {
        StandbyMode::Edge
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SavedDock {
    x: i32,
    y: i32,
    edge: DockEdge,
    #[serde(default)]
    standby_mode: StandbyMode,
    #[serde(default)]
    orb_x: Option<i32>,
    #[serde(default)]
    orb_y: Option<i32>,
}

#[derive(Clone, Debug, Serialize)]
struct DockStatePayload {
    edge: DockEdge,
    standby_mode: StandbyMode,
    tucked: bool,
    orb: bool,
    play: bool,
    seam: bool,
    idle: bool,
    pinned: bool,
    preview: DockEdge,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ImpactEvent {
    axis: &'static str,
    normal_speed: f64,
}

#[derive(Clone, Debug, Serialize)]
struct OrbFxPayload {
    angle: f64,
    impact_axis: Option<&'static str>,
    impact_speed: f64,
    combo: u32,
}

#[derive(Clone, Debug, Default)]
struct VelocityTracker {
    samples: VecDeque<(f64, f64, f64)>,
}

impl VelocityTracker {
    fn clear(&mut self) {
        self.samples.clear();
    }

    fn push(&mut self, t: f64, x: f64, y: f64) {
        while let Some(&(old_t, _, _)) = self.samples.front() {
            if t - old_t > 0.085 || self.samples.len() >= 10 {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        self.samples.push_back((t, x, y));
    }

    fn velocity(&self, max_speed: f64) -> (f64, f64) {
        let Some(&(t_last, x_last, y_last)) = self.samples.back() else {
            return (0.0, 0.0);
        };
        let mut anchor = None;
        for &(t, x, y) in self.samples.iter() {
            let dt = t_last - t;
            if dt >= 0.006 {
                anchor = Some((t, x, y));
                if dt <= 0.065 {
                    break;
                }
            }
        }
        let Some((t0, x0, y0)) = anchor else {
            return (0.0, 0.0);
        };
        let dt = (t_last - t0).max(0.006);
        let mut vx = (x_last - x0) / dt;
        let mut vy = (y_last - y0) / dt;
        let speed = vx.hypot(vy);
        if speed > max_speed && speed > 0.0 {
            let k = max_speed / speed;
            vx *= k;
            vy *= k;
        }
        (vx, vy)
    }
}

#[derive(Clone, Copy, Debug)]
struct OrbPhysicsBounds {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

impl OrbPhysicsBounds {
    fn from_work_area(area: WorkRect, orb_size: i32, inset: i32) -> Self {
        let min_x = (area.x - inset) as f64;
        let max_x = (area.right() - orb_size + inset) as f64;
        let min_y = (area.y - inset) as f64;
        let max_y = (area.bottom() - orb_size + inset) as f64;
        Self {
            min_x,
            max_x: max_x.max(min_x),
            min_y,
            max_y: max_y.max(min_y),
        }
    }
}

#[derive(Clone, Debug)]
struct OrbPhysics {
    px: f64,
    py: f64,
    vx: f64,
    vy: f64,
    omega: f64,
    angle_deg: f64,
    sleeping: bool,
    combo: u32,
}

impl Default for OrbPhysics {
    fn default() -> Self {
        Self {
            px: 0.0,
            py: 0.0,
            vx: 0.0,
            vy: 0.0,
            omega: 0.0,
            angle_deg: 0.0,
            sleeping: true,
            combo: 0,
        }
    }
}

struct DockInner {
    epoch: Instant,
    edge: DockEdge,
    standby_mode: StandbyMode,
    dock_x: i32,
    dock_y: i32,
    orb_x: Option<i32>,
    orb_y: Option<i32>,
    pod_w_phys: i32,
    pod_h_phys: i32,
    work_area: WorkRect,
    tucked: bool,
    orb_collapsed: bool,
    play_mode: bool,
    orb_coasting: bool,
    custom_dragging: bool,
    drag_offset: (i32, i32),
    vel_tracker: VelocityTracker,
    physics: OrbPhysics,
    last_physics_tick: Instant,
    last_kick_at: Instant,
    last_cursor: Option<(Instant, f64, f64)>,
    last_emitted_angle: f64,
    orb_hover_armed: bool,
    orb_hover_since: Option<Instant>,
    orb_expand_suppress_until: Instant,
    pre_play_orb_pos: Option<(i32, i32)>,
    pre_play_standby: Option<StandbyMode>,
    pending_orb_return: Option<(i32, i32)>,
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
            epoch: now,
            edge: DockEdge::Top,
            standby_mode: StandbyMode::Edge,
            dock_x: 0,
            dock_y: 0,
            orb_x: None,
            orb_y: None,
            pod_w_phys: 480,
            pod_h_phys: 64,
            work_area: WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            tucked: false,
            orb_collapsed: false,
            play_mode: false,
            orb_coasting: false,
            custom_dragging: false,
            drag_offset: (24, 24),
            vel_tracker: VelocityTracker::default(),
            physics: OrbPhysics::default(),
            last_physics_tick: now,
            last_kick_at: now,
            last_cursor: None,
            last_emitted_angle: 0.0,
            orb_hover_armed: true,
            orb_hover_since: None,
            orb_expand_suppress_until: now,
            pre_play_orb_pos: None,
            pre_play_standby: None,
            pending_orb_return: None,
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
            standby_mode: self.standby_mode,
            tucked: self.tucked,
            orb: self.orb_collapsed,
            play: self.play_mode,
            seam: self.seam_mode,
            idle: self.idle,
            pinned: self.pinned,
            preview: self.preview,
        }
    }

    fn to_saved(&self) -> SavedDock {
        SavedDock {
            x: self.dock_x,
            y: self.dock_y,
            edge: self.edge,
            standby_mode: self.standby_mode,
            orb_x: self.orb_x,
            orb_y: self.orb_y,
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
        .env("ENVGUARD_APP_COMMIT", env!("ENVGUARD_GIT_SHA"))
        .env("ENVGUARD_APP_COMMIT_UNIX", env!("ENVGUARD_GIT_COMMIT_UNIX"))
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

    let mut val: serde_json::Value =
        serde_json::from_str(json_line).map_err(|e| format!("状态 JSON 无效: {e}"))?;
    if let Some(obj) = val.as_object_mut() {
        obj.insert(
            "git_short_sha".to_string(),
            serde_json::Value::String(env!("ENVGUARD_GIT_SHORT_SHA").to_string()),
        );
    }
    Ok(val)
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

fn sibling_path_with_suffix(path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

fn cleanup_stale_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let old_exe = sibling_path_with_suffix(&exe, ".old");
        let _ = std::fs::remove_file(old_exe);
        if let Some(dir) = exe.parent() {
            let _ = std::fs::remove_file(dir.join("claude-envguard.exe.old"));
            let _ = std::fs::remove_file(dir.join("claude-envguard.exe.new"));
        }
    }
}

#[tauri::command]
async fn check_app_update(app: AppHandle) -> Result<serde_json::Value, String> {
    let res = run_core_async(&app, vec!["check-app-update".to_string()]).await?;
    let json_line = res
        .output
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| format!("检查更新返回无效: {}", res.output))?;
    serde_json::from_str(json_line).map_err(|e| format!("解析更新状态失败: {e}"))
}

#[tauri::command]
async fn apply_app_update(
    app: AppHandle,
    guard: State<'_, OpGuard>,
) -> Result<CommandOutcome, String> {
    let _lease = BusyLease::try_acquire(&guard.busy)?;
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("无法获取当前程序路径: {e}"))?;
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| "无法定位当前程序所在目录".to_string())?
        .to_path_buf();

    let outcome = run_core_async(
        &app,
        vec![
            "stage-app-update".to_string(),
            exe_dir.to_string_lossy().into_owned(),
        ],
    )
    .await?;

    if outcome.exit_code != 0 {
        return Ok(outcome);
    }

    let staged_exe = exe_dir.join("claude-envguard.exe.new");
    if !staged_exe.is_file() {
        return Ok(outcome);
    }

    let old_exe = sibling_path_with_suffix(&current_exe, ".old");
    let _ = std::fs::remove_file(&old_exe);

    std::fs::rename(&current_exe, &old_exe).map_err(|e| {
        let _ = std::fs::remove_file(&staged_exe);
        format!("重命名当前运行程序失败: {e}")
    })?;

    if let Err(e) = std::fs::rename(&staged_exe, &current_exe) {
        let _ = std::fs::rename(&old_exe, &current_exe);
        let _ = std::fs::remove_file(&staged_exe);
        return Err(format!("替换新版可执行文件失败(已回滚): {e}"));
    }

    let mut relaunch = Command::new("powershell.exe");
    relaunch
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "try { Wait-Process -Id ([int]$env:_EG_OLD_PID) -Timeout 12 -ErrorAction SilentlyContinue } catch {}; Start-Sleep -Milliseconds 120; Remove-Item -LiteralPath $env:_EG_OLD_EXE -Force -ErrorAction SilentlyContinue; Start-Process -FilePath $env:_EG_NEW_EXE",
        ])
        .env("_EG_OLD_PID", std::process::id().to_string())
        .env("_EG_OLD_EXE", &old_exe)
        .env("_EG_NEW_EXE", &current_exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(target_os = "windows")]
    relaunch.creation_flags(CREATE_NO_WINDOW);

    if let Err(e) = relaunch.spawn() {
        // Restore original executable name if helper process could not spawn
        let _ = std::fs::remove_file(&current_exe);
        let _ = std::fs::rename(&old_exe, &current_exe);
        return Err(format!("启动热更新重启助手失败(已回滚): {e}"));
    }

    let quitting_flag = Arc::clone(&guard.quitting);
    let app_for_exit = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(220));
        quitting_flag.store(true, Ordering::SeqCst);
        app_for_exit.exit(0);
    });

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

const PEEK_LOGICAL_PX: f64 = 9.0;
const SNAP_THRESHOLD_LOGICAL_PX: f64 = 48.0;
const POD_WIDTH_LOGICAL_PX: f64 = 480.0;
const POD_BASE_HEIGHT_LOGICAL_PX: f64 = 64.0;
const ORB_SIZE_LOGICAL_PX: f64 = 72.0;
const ORB_VISUAL_RADIUS_LOGICAL_PX: f64 = 28.0;
const ORB_INSET_LOGICAL_PX: f64 = 8.0;
const ORB_HOVER_EXPAND_MS: u64 = 260;

fn closest_point_on_segment(ax: f64, ay: f64, bx: f64, by: f64, px: f64, py: f64) -> (f64, f64) {
    let abx = bx - ax;
    let aby = by - ay;
    let len_sq = abx * abx + aby * aby;
    if len_sq <= 1e-9 {
        return (ax, ay);
    }
    let t = (((px - ax) * abx + (py - ay) * aby) / len_sq).clamp(0.0, 1.0);
    (ax + abx * t, ay + aby * t)
}

fn step_orb_gravity(
    phys: &mut OrbPhysics,
    dt: f64,
    bounds: OrbPhysicsBounds,
    scale: f64,
) -> Option<ImpactEvent> {
    if phys.sleeping {
        return None;
    }
    let dt = dt.clamp(0.001, 0.032);
    let scale = scale.max(0.5);
    let gravity = 2250.0 * scale;
    let max_speed = 4600.0 * scale;
    let radius = (ORB_VISUAL_RADIUS_LOGICAL_PX * scale).max(16.0);

    phys.vy += gravity * dt;
    phys.vx *= (1.0 - 0.16 * dt).max(0.0);
    phys.vy *= (1.0 - 0.06 * dt).max(0.0);

    let speed = phys.vx.hypot(phys.vy);
    if speed > max_speed && speed > 0.0 {
        let k = max_speed / speed;
        phys.vx *= k;
        phys.vy *= k;
    }

    phys.px += phys.vx * dt;
    phys.py += phys.vy * dt;

    let mut best_impact: Option<ImpactEvent> = None;
    let mut consider_impact = |axis: &'static str, raw_speed_phys: f64| {
        let norm_logical = raw_speed_phys / scale;
        if norm_logical >= 95.0 {
            if best_impact.map_or(true, |b| norm_logical > b.normal_speed) {
                best_impact = Some(ImpactEvent {
                    axis,
                    normal_speed: norm_logical,
                });
            }
        }
    };

    if phys.px < bounds.min_x {
        phys.px = bounds.min_x;
        let hit_v = phys.vx.abs();
        if phys.vx < 0.0 {
            phys.vx = -phys.vx * 0.78;
        }
        phys.omega = (phys.vy / radius) * 0.55;
        consider_impact("x", hit_v);
    } else if phys.px > bounds.max_x {
        phys.px = bounds.max_x;
        let hit_v = phys.vx.abs();
        if phys.vx > 0.0 {
            phys.vx = -phys.vx * 0.78;
        }
        phys.omega = -(phys.vy / radius) * 0.55;
        consider_impact("x", hit_v);
    }

    if phys.py < bounds.min_y {
        phys.py = bounds.min_y;
        let hit_v = phys.vy.abs();
        if phys.vy < 0.0 {
            phys.vy = -phys.vy * 0.72;
        }
        consider_impact("y", hit_v);
    }

    if phys.py >= bounds.max_y {
        phys.py = bounds.max_y;
        let hit_v = phys.vy.abs();
        phys.combo = 0;

        if phys.vy > 105.0 * scale {
            phys.vy = -phys.vy * 0.75;
            phys.vx *= 0.94;
            phys.omega = phys.vx / radius;
            consider_impact("y", hit_v);
        } else {
            phys.vy = 0.0;
            let decel = 320.0 * scale * dt;
            if phys.vx.abs() <= decel {
                phys.vx = 0.0;
            } else {
                phys.vx -= phys.vx.signum() * decel;
            }
            phys.vx *= (1.0 - 2.4 * dt).max(0.0);
            phys.omega = phys.vx / radius;
            if phys.vx.abs() < 8.0 * scale {
                phys.vx = 0.0;
                phys.omega = 0.0;
                phys.sleeping = true;
            }
        }
    } else {
        phys.omega *= (1.0 - 0.45 * dt).max(0.0);
    }

    if !phys.sleeping {
        phys.angle_deg = (phys.angle_deg + phys.omega.to_degrees() * dt) % 360.0;
    }

    best_impact
}

fn apply_cursor_kick(
    phys: &mut OrbPhysics,
    prev_cur: (f64, f64),
    cur: (f64, f64),
    dt: f64,
    orb_size: f64,
    bounds: OrbPhysicsBounds,
    scale: f64,
) -> Option<ImpactEvent> {
    if dt <= 0.0005 || dt > 0.15 {
        return None;
    }
    let scale = scale.max(0.5);
    let cvx = (cur.0 - prev_cur.0) / dt;
    let cvy = (cur.1 - prev_cur.1) / dt;
    let cspeed = cvx.hypot(cvy);

    let on_floor = phys.sleeping || (phys.py >= bounds.max_y - 2.0 && phys.vy.abs() < 45.0 * scale);
    let min_kick_speed = if on_floor {
        600.0 * scale
    } else {
        310.0 * scale
    };
    if cspeed < min_kick_speed {
        return None;
    }

    let bcx = phys.px + orb_size * 0.5;
    let bcy = phys.py + orb_size * 0.5;
    let r = (ORB_VISUAL_RADIUS_LOGICAL_PX * scale).max(16.0);

    // Require outside-in cursor strike so resting or wiggling cursor inside the ball never machine-gun kicks it
    let dist_prev = (prev_cur.0 - bcx).hypot(prev_cur.1 - bcy);
    if dist_prev < r * 0.84 {
        return None;
    }

    let (qx, qy) = closest_point_on_segment(prev_cur.0, prev_cur.1, cur.0, cur.1, bcx, bcy);
    let seg_dist = (bcx - qx).hypot(bcy - qy);
    let pad = if on_floor { 4.0 * scale } else { 7.0 * scale };
    if seg_dist > r + pad {
        return None;
    }

    let dx = bcx - qx;
    let dy = bcy - qy;
    let dlen = dx.hypot(dy);
    let (nx, ny) = if dlen > 1e-4 {
        (dx / dlen, dy / dlen)
    } else {
        (0.0, -1.0)
    };

    let kick_vx = (cvx * 0.62 + nx * 240.0 * scale).clamp(-3600.0 * scale, 3600.0 * scale);
    let raw_vy = cvy * 0.62 + ny * 340.0 * scale - 290.0 * scale;
    let kick_vy = raw_vy
        .clamp(-3600.0 * scale, 2200.0 * scale)
        .min(-430.0 * scale);

    phys.vx = kick_vx;
    phys.vy = kick_vy;
    phys.omega = (kick_vx / r) * 0.92;
    phys.sleeping = false;
    phys.combo = phys.combo.saturating_add(1);

    Some(ImpactEvent {
        axis: if nx.abs() > ny.abs() { "x" } else { "y" },
        normal_speed: ((cspeed * 0.72) / scale).clamp(420.0, 2200.0),
    })
}

fn compute_visible_pod_pos(
    dock_x: i32,
    dock_y: i32,
    ww: i32,
    wh: i32,
    area: WorkRect,
    edge: DockEdge,
    margin: i32,
) -> (i32, i32) {
    let min_x = area.x + margin;
    let max_x = (area.right() - ww - margin).max(min_x);
    let min_y = area.y + margin;
    let max_y = (area.bottom() - wh - margin).max(min_y);

    let x = match edge {
        DockEdge::Left => area.x,
        DockEdge::Right => area.right() - ww,
        DockEdge::Top | DockEdge::None => dock_x.clamp(min_x, max_x),
    };
    let y = match edge {
        DockEdge::Top => area.y,
        DockEdge::Left | DockEdge::Right | DockEdge::None => dock_y.clamp(min_y, max_y),
    };
    (x, y)
}

fn compute_orb_pos_from_pod(
    dock_x: i32,
    dock_y: i32,
    pod_w: i32,
    pod_h: i32,
    orb_size: i32,
    area: WorkRect,
    edge: DockEdge,
    margin: i32,
) -> (i32, i32) {
    let min_x = area.x + margin;
    let max_x = (area.right() - orb_size - margin).max(min_x);
    let min_y = area.y + margin;
    let max_y = (area.bottom() - orb_size - margin).max(min_y);

    match edge {
        DockEdge::Top => (
            (dock_x + (pod_w - orb_size) / 2).clamp(min_x, max_x),
            area.y + margin,
        ),
        DockEdge::Left => (
            area.x + margin,
            (dock_y + (pod_h - orb_size) / 2).clamp(min_y, max_y),
        ),
        DockEdge::Right => (
            area.right() - orb_size - margin,
            (dock_y + (pod_h - orb_size) / 2).clamp(min_y, max_y),
        ),
        DockEdge::None => (
            (dock_x + (pod_w - orb_size) / 2).clamp(min_x, max_x),
            (dock_y + (pod_h - orb_size) / 2).clamp(min_y, max_y),
        ),
    }
}

fn compute_pod_pos_from_orb(
    orb_x: i32,
    orb_y: i32,
    orb_size: i32,
    pod_w: i32,
    pod_h: i32,
    area: WorkRect,
    margin: i32,
) -> (i32, i32, DockEdge) {
    let near = margin + (24).max(orb_size / 3);
    let d_top = orb_y - area.y;
    let d_left = orb_x - area.x;
    let d_right = area.right() - (orb_x + orb_size);

    let min_x = area.x + margin;
    let max_x = (area.right() - pod_w - margin).max(min_x);
    let min_y = area.y + margin;
    let max_y = (area.bottom() - pod_h - margin).max(min_y);

    let candidates = [
        (d_top, DockEdge::Top),
        (d_left, DockEdge::Left),
        (d_right, DockEdge::Right),
    ];
    if let Some(&(min_d, edge)) = candidates.iter().min_by_key(|c| c.0) {
        if min_d <= near {
            return match edge {
                DockEdge::Top => (
                    (orb_x + orb_size / 2 - pod_w / 2).clamp(min_x, max_x),
                    area.y,
                    DockEdge::Top,
                ),
                DockEdge::Left => (
                    area.x,
                    (orb_y + orb_size / 2 - pod_h / 2).clamp(min_y, max_y),
                    DockEdge::Left,
                ),
                DockEdge::Right => (
                    area.right() - pod_w,
                    (orb_y + orb_size / 2 - pod_h / 2).clamp(min_y, max_y),
                    DockEdge::Right,
                ),
                DockEdge::None => unreachable!(),
            };
        }
    }
    (
        (orb_x + orb_size / 2 - pod_w / 2).clamp(min_x, max_x),
        (orb_y + orb_size / 2 - pod_h / 2).clamp(min_y, max_y),
        DockEdge::None,
    )
}

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
            let half_w = (76.0 * scale).round() as i32;
            cx >= center_x - half_w
                && cx <= center_x + half_w
                && cy >= area.y - 4
                && cy <= area.y + depth
        }
        DockEdge::Left => {
            let center_y = dock_y + wh / 2;
            let half_h = (28.0 * scale).round() as i32;
            cx >= area.x - 4
                && cx <= area.x + depth
                && cy >= center_y - half_h
                && cy <= center_y + half_h
        }
        DockEdge::Right => {
            let center_y = dock_y + wh / 2;
            let half_h = (28.0 * scale).round() as i32;
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
    set_window_pos_fast(window, dock, x, y);
}

fn set_window_pos_fast(
    window: &WebviewWindow,
    dock: &Arc<Mutex<DockInner>>,
    x: i32,
    y: i32,
) {
    {
        let mut st = dock.lock().unwrap();
        st.last_programmatic_target = Some((x, y));
        st.ignore_moves_until = Instant::now() + Duration::from_millis(220);
    }
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        extern "system" {
            fn SetWindowPos(
                hwnd: *mut std::ffi::c_void,
                hwnd_insert_after: *mut std::ffi::c_void,
                x: i32,
                y: i32,
                cx: i32,
                cy: i32,
                u_flags: u32,
            ) -> i32;
        }
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOZORDER: u32 = 0x0004;
        const SWP_NOACTIVATE: u32 = 0x0010;
        if let Ok(hwnd) = window.hwnd() {
            // SAFETY: Moves HWND directly in DWM without triggering WM_SIZE or DragFullWindows=0 wireframe.
            unsafe {
                SetWindowPos(
                    hwnd.0,
                    std::ptr::null_mut(),
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            return;
        }
    }
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn set_window_rect_atomic(
    window: &WebviewWindow,
    dock: &Arc<Mutex<DockInner>>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) {
    {
        let mut st = dock.lock().unwrap();
        st.last_programmatic_target = Some((x, y));
        st.ignore_moves_until = Instant::now() + Duration::from_millis(280);
    }
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        extern "system" {
            fn SetWindowPos(
                hwnd: *mut std::ffi::c_void,
                hwnd_insert_after: *mut std::ffi::c_void,
                x: i32,
                y: i32,
                cx: i32,
                cy: i32,
                u_flags: u32,
            ) -> i32;
        }
        const SWP_NOZORDER: u32 = 0x0004;
        const SWP_NOACTIVATE: u32 = 0x0010;
        if let Ok(hwnd) = window.hwnd() {
            // SAFETY: Single atomic Win32 SetWindowPos updates (x, y, w, h) in one DWM frame
            // without altering Z-order or stealing foreground activation.
            unsafe {
                SetWindowPos(
                    hwnd.0,
                    std::ptr::null_mut(),
                    x,
                    y,
                    w.max(1),
                    h.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            return;
        }
    }
    let _ = window.set_position(PhysicalPosition::new(x, y));
    let _ = window.set_size(PhysicalSize::new(w.max(1) as u32, h.max(1) as u32));
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
    let scale = window.scale_factor().unwrap_or(1.0);
    let pod_w = (POD_WIDTH_LOGICAL_PX * scale).round() as i32;
    let pod_h = (POD_BASE_HEIGHT_LOGICAL_PX * scale).round() as i32;
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
        let x = area.x + ((area.w - pod_w) / 2).max(0);
        let y = area.y;
        let seam = has_adjacent_monitor_beyond_edge(window, x, y, pod_w, pod_h, area, DockEdge::Top);
        let now = Instant::now();
        let (payload, saved) = {
            let mut st = dock.lock().unwrap();
            st.edge = DockEdge::Top;
            st.dock_x = x;
            st.dock_y = y;
            st.orb_x = None;
            st.orb_y = None;
            st.pod_w_phys = pod_w;
            st.pod_h_phys = pod_h;
            st.work_area = area;
            st.seam_mode = seam;
            st.tucked = false;
            st.orb_collapsed = false;
            st.play_mode = false;
            st.orb_coasting = false;
            st.custom_dragging = false;
            st.pending_orb_return = None;
            st.physics = OrbPhysics::default();
            st.idle = false;
            st.preview = DockEdge::None;
            st.pending_user_move = false;
            st.force_tuck = false;
            st.force_untuck = false;
            st.last_hover_at = now;
            st.grace_until = now + Duration::from_millis(2800);
            st.ignore_moves_until = now + Duration::from_millis(320);
            (st.payload(), st.to_saved())
        };
        let _ = window.set_ignore_cursor_events(false);
        let _ = window.set_size(PhysicalSize::new(pod_w as u32, pod_h as u32));
        set_programmatic_pos(window, dock, x, y);
        if persist {
            persist_saved_dock(&saved);
        }
        let _ = window.emit("dock-state", payload);
    }
}

fn init_window_dock(window: &WebviewWindow, dock: &Arc<Mutex<DockInner>>) {
    let _ = window.set_always_on_top(true);
    let scale = window.scale_factor().unwrap_or(1.0);
    let ww = (POD_WIDTH_LOGICAL_PX * scale).round() as i32;
    let wh = (POD_BASE_HEIGHT_LOGICAL_PX * scale).round() as i32;
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
                    st.standby_mode = saved.standby_mode;
                    st.dock_x = x;
                    st.dock_y = y;
                    st.orb_x = saved.orb_x;
                    st.orb_y = saved.orb_y;
                    st.pod_w_phys = ww;
                    st.pod_h_phys = wh;
                    st.work_area = area;
                    st.seam_mode = seam;
                    st.tucked = false;
                    st.orb_collapsed = false;
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
    MoveFast {
        to: (i32, i32),
        payload: Option<DockStatePayload>,
        fx: Option<OrbFxPayload>,
    },
    GlideOrbReturn {
        from: (i32, i32),
        to: (i32, i32),
        payload: DockStatePayload,
        fx: OrbFxPayload,
    },
    SnapPod {
        to: (i32, i32),
        saved: SavedDock,
        payload: DockStatePayload,
    },
    SaveOrbDrag {
        to: (i32, i32),
        saved: SavedDock,
        payload: DockStatePayload,
    },
    AdjustOnScreen {
        to: (i32, i32),
    },
    TuckEdge {
        edge: DockEdge,
        dock_x: i32,
        dock_y: i32,
        area: WorkRect,
        seam: bool,
        peek: i32,
    },
    UntuckEdge {
        from: (i32, i32),
        to: (i32, i32),
        payload: DockStatePayload,
    },
    EnterOrb {
        orb_pos: (i32, i32),
        orb_size: i32,
        play: bool,
    },
    ExitOrb {
        pod_pos: (i32, i32),
        pod_w: i32,
        pod_h: i32,
        payload: DockStatePayload,
    },
    EmitOnly(DockStatePayload),
    EmitOrbFx(OrbFxPayload),
}

fn start_dock_watcher(
    window: WebviewWindow,
    dock: Arc<Mutex<DockInner>>,
    quitting: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        while !quitting.load(Ordering::SeqCst) {
            let sleep_ms = {
                let st = dock.lock().unwrap();
                if st.custom_dragging
                    || st.orb_coasting
                    || (st.play_mode && st.orb_collapsed && !st.physics.sleeping)
                {
                    8
                } else if st.play_mode && st.orb_collapsed {
                    16
                } else {
                    30
                }
            };
            std::thread::sleep(Duration::from_millis(sleep_ms));
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
            let orb_size = (ORB_SIZE_LOGICAL_PX * scale).round() as i32;
            let orb_inset = (ORB_INSET_LOGICAL_PX * scale).round() as i32;
            let pod_default_w = (POD_WIDTH_LOGICAL_PX * scale).round() as i32;
            let pod_base_h = (POD_BASE_HEIGHT_LOGICAL_PX * scale).round() as i32;
            let mouse_down = is_primary_mouse_down();
            let now = Instant::now();

            let mut action = WatcherAction::None;

            {
                let mut st = dock.lock().unwrap();
                if st.animating {
                    continue;
                }
                let prev_cursor = st.last_cursor;
                st.last_cursor = Some((now, cur.x, cur.y));

                if !st.orb_collapsed && !st.tucked && ww >= (pod_default_w - 20) {
                    st.pod_w_phys = ww;
                    if wh <= pod_base_h + (24.0 * scale).round() as i32 {
                        st.pod_h_phys = wh;
                    }
                }

                // 0. Smooth return glide after exiting Gravity Play Mode back to parked Orb spot
                if let Some(return_to) = st.pending_orb_return.take() {
                    st.play_mode = false;
                    st.orb_coasting = false;
                    st.custom_dragging = false;
                    st.physics = OrbPhysics::default();
                    st.last_emitted_angle = 0.0;
                    st.orb_hover_since = None;
                    st.orb_hover_armed = false;
                    st.orb_expand_suppress_until = now + Duration::from_millis(520);
                    action = WatcherAction::GlideOrbReturn {
                        from: (wx, wy),
                        to: return_to,
                        payload: st.payload(),
                        fx: OrbFxPayload {
                            angle: 0.0,
                            impact_axis: None,
                            impact_speed: 0.0,
                            combo: 0,
                        },
                    };
                }
                // 1. Live 120Hz Custom Dragging (eliminates Win32 DragFullWindows=0 hollow black box!)
                else if st.custom_dragging {
                    let target_x = cx - st.drag_offset.0;
                    let target_y = cy - st.drag_offset.1;
                    let t_sec = now.duration_since(st.epoch).as_secs_f64();
                    let live_area = monitor_work_rect(
                        &window,
                        target_x + ww / 2,
                        target_y + (wh.min(160)) / 2,
                    )
                    .unwrap_or(st.work_area);
                    st.work_area = live_area;
                    st.last_hover_at = now;
                    st.orb_hover_since = None;

                    if mouse_down {
                        st.vel_tracker.push(t_sec, target_x as f64, target_y as f64);
                        if st.orb_collapsed {
                            let bounds =
                                OrbPhysicsBounds::from_work_area(live_area, orb_size, orb_inset);
                            let clamped_x =
                                (target_x as f64).clamp(bounds.min_x, bounds.max_x).round() as i32;
                            let clamped_y =
                                (target_y as f64).clamp(bounds.min_y, bounds.max_y).round() as i32;
                            st.physics.px = clamped_x as f64;
                            st.physics.py = clamped_y as f64;
                            action = WatcherAction::MoveFast {
                                to: (clamped_x, clamped_y),
                                payload: None,
                                fx: None,
                            };
                        } else {
                            let prev =
                                detect_snap_edge(target_x, target_y, ww, wh, live_area, threshold);
                            let mut emit_payload = None;
                            if prev != st.preview {
                                st.preview = prev;
                                emit_payload = Some(st.payload());
                            }
                            action = WatcherAction::MoveFast {
                                to: (target_x, target_y),
                                payload: emit_payload,
                                fx: None,
                            };
                        }
                    } else {
                        // Mouse button released -> finish custom drag with velocity handoff!
                        st.custom_dragging = false;
                        st.pending_user_move = false;
                        let (vx, vy) = st.vel_tracker.velocity(4300.0 * scale);
                        st.vel_tracker.clear();

                        if st.orb_collapsed {
                            let bounds =
                                OrbPhysicsBounds::from_work_area(live_area, orb_size, orb_inset);
                            let ox = (wx as f64).clamp(bounds.min_x, bounds.max_x);
                            let oy = (wy as f64).clamp(bounds.min_y, bounds.max_y);

                            if st.play_mode {
                                // Throw / fling directly into 2D gravity physics!
                                let r = (ORB_VISUAL_RADIUS_LOGICAL_PX * scale).max(16.0);
                                st.physics.px = ox;
                                st.physics.py = oy;
                                st.physics.vx = vx;
                                st.physics.vy = vy;
                                st.physics.omega = (vx / r) * 0.65;
                                st.physics.sleeping = false;
                                st.last_physics_tick = now;
                            } else if vx.hypot(vy) >= 240.0 * scale {
                                // Playful damped glide & bounce in normal Orb standby before parking!
                                st.orb_coasting = true;
                                st.physics.px = ox;
                                st.physics.py = oy;
                                st.physics.vx = vx * 0.62;
                                st.physics.vy = vy * 0.62;
                                st.physics.sleeping = false;
                                st.last_physics_tick = now;
                                st.orb_hover_armed = false;
                            } else {
                                let min_ox = live_area.x + margin;
                                let max_ox = (live_area.right() - orb_size - margin).max(min_ox);
                                let min_oy = live_area.y + margin;
                                let max_oy = (live_area.bottom() - orb_size - margin).max(min_oy);
                                let parked_x = (ox.round() as i32).clamp(min_ox, max_ox);
                                let parked_y = (oy.round() as i32).clamp(min_oy, max_oy);
                                let pod_w = st.pod_w_phys.max(pod_default_w);
                                let pod_h = st.pod_h_phys.max(pod_base_h);
                                let (px, py, new_edge) = compute_pod_pos_from_orb(
                                    parked_x, parked_y, orb_size, pod_w, pod_h, live_area, margin,
                                );
                                st.orb_x = Some(parked_x);
                                st.orb_y = Some(parked_y);
                                st.dock_x = px;
                                st.dock_y = py;
                                st.edge = new_edge;
                                st.seam_mode = has_adjacent_monitor_beyond_edge(
                                    &window, px, py, pod_w, pod_h, live_area, new_edge,
                                );
                                st.orb_hover_armed = false;
                                st.orb_expand_suppress_until = now + Duration::from_millis(450);
                                action = WatcherAction::SaveOrbDrag {
                                    to: (parked_x, parked_y),
                                    saved: st.to_saved(),
                                    payload: st.payload(),
                                };
                            }
                        } else {
                            st.preview = DockEdge::None;
                            let anchor_h = st.pod_h_phys.max(pod_base_h);
                            let proj_x = wx + (vx * 0.055).round() as i32;
                            let proj_y = wy + (vy * 0.055).round() as i32;
                            let (sx, sy, new_edge) = snap_window_rect(
                                proj_x, proj_y, ww, anchor_h, live_area, threshold, margin,
                            );
                            let seam = has_adjacent_monitor_beyond_edge(
                                &window, sx, sy, ww, anchor_h, live_area, new_edge,
                            );
                            st.edge = new_edge;
                            st.dock_x = sx;
                            st.dock_y = sy;
                            st.seam_mode = seam;
                            let (ox, oy) = compute_orb_pos_from_pod(
                                sx, sy, ww, anchor_h, orb_size, live_area, new_edge, margin,
                            );
                            st.orb_x = Some(ox);
                            st.orb_y = Some(oy);
                            st.tucked = false;
                            st.orb_collapsed = false;
                            st.idle = false;
                            st.last_hover_at = now;
                            st.grace_until = now + Duration::from_millis(1400);
                            let vis_to = compute_visible_pod_pos(
                                sx, sy, ww, wh, live_area, new_edge, margin,
                            );
                            action = WatcherAction::SnapPod {
                                to: vis_to,
                                saved: st.to_saved(),
                                payload: st.payload(),
                            };
                        }
                    }
                } else if st.force_untuck && (st.tucked || st.orb_collapsed) {
                    st.force_untuck = false;
                    st.force_tuck = false;
                    st.play_mode = false;
                    st.orb_coasting = false;
                    st.physics = OrbPhysics::default();
                    st.idle = false;
                    st.last_hover_at = now;
                    st.grace_until = now + Duration::from_millis(2400);
                    if let Some(prev_standby) = st.pre_play_standby.take() {
                        st.standby_mode = prev_standby;
                    }
                    st.pre_play_orb_pos = None;
                    if st.orb_collapsed {
                        st.orb_collapsed = false;
                        st.orb_hover_since = None;
                        let pod_w = st.pod_w_phys.max(pod_default_w);
                        let pod_h = st.pod_h_phys.max(pod_base_h);
                        let to = compute_visible_pod_pos(
                            st.dock_x,
                            st.dock_y,
                            pod_w,
                            pod_h,
                            st.work_area,
                            st.edge,
                            margin,
                        );
                        action = WatcherAction::ExitOrb {
                            pod_pos: to,
                            pod_w,
                            pod_h,
                            payload: st.payload(),
                        };
                    } else {
                        st.tucked = false;
                        let to = (st.dock_x, st.dock_y);
                        action = WatcherAction::UntuckEdge {
                            from: (wx, wy),
                            to,
                            payload: st.payload(),
                        };
                    }
                } else if st.force_tuck && !st.tucked && !st.orb_collapsed {
                    st.force_tuck = false;
                    st.pinned = false;
                    st.ui_hold = false;
                    if st.standby_mode == StandbyMode::Orb || st.play_mode {
                        let pod_w = st.pod_w_phys.max(pod_default_w);
                        let pod_h = st.pod_h_phys.max(pod_base_h);
                        let opos = if st.play_mode {
                            (st.physics.px.round() as i32, st.physics.py.round() as i32)
                        } else {
                            match (st.orb_x, st.orb_y) {
                                (Some(ox), Some(oy)) => (ox, oy),
                                _ => {
                                    let p = compute_orb_pos_from_pod(
                                        st.dock_x,
                                        st.dock_y,
                                        pod_w,
                                        pod_h,
                                        orb_size,
                                        st.work_area,
                                        st.edge,
                                        margin,
                                    );
                                    st.orb_x = Some(p.0);
                                    st.orb_y = Some(p.1);
                                    p
                                }
                            }
                        };
                        st.last_physics_tick = now;
                        action = WatcherAction::EnterOrb {
                            orb_pos: opos,
                            orb_size,
                            play: st.play_mode,
                        };
                    } else if st.edge != DockEdge::None {
                        action = WatcherAction::TuckEdge {
                            edge: st.edge,
                            dock_x: st.dock_x,
                            dock_y: st.dock_y,
                            area: st.work_area,
                            seam: st.seam_mode,
                            peek,
                        };
                    }
                } else if st.pending_user_move {
                    let live_area = monitor_work_rect(&window, wx + ww / 2, wy + (wh.min(160)) / 2)
                        .unwrap_or(st.work_area);
                    if st.orb_collapsed {
                        if !mouse_down {
                            st.pending_user_move = false;
                            let min_ox = live_area.x + margin;
                            let max_ox = (live_area.right() - orb_size - margin).max(min_ox);
                            let min_oy = live_area.y + margin;
                            let max_oy = (live_area.bottom() - orb_size - margin).max(min_oy);
                            let ox = wx.clamp(min_ox, max_ox);
                            let oy = wy.clamp(min_oy, max_oy);
                            let pod_w = st.pod_w_phys.max(pod_default_w);
                            let pod_h = st.pod_h_phys.max(pod_base_h);
                            let (px, py, new_edge) = compute_pod_pos_from_orb(
                                ox, oy, orb_size, pod_w, pod_h, live_area, margin,
                            );
                            st.orb_x = Some(ox);
                            st.orb_y = Some(oy);
                            st.dock_x = px;
                            st.dock_y = py;
                            st.edge = new_edge;
                            st.work_area = live_area;
                            st.seam_mode = has_adjacent_monitor_beyond_edge(
                                &window, px, py, pod_w, pod_h, live_area, new_edge,
                            );
                            action = WatcherAction::SaveOrbDrag {
                                to: (ox, oy),
                                saved: st.to_saved(),
                                payload: st.payload(),
                            };
                        }
                    } else if mouse_down {
                        let prev = detect_snap_edge(wx, wy, ww, wh, live_area, threshold);
                        if prev != st.preview {
                            st.preview = prev;
                            action = WatcherAction::EmitOnly(st.payload());
                        }
                    } else {
                        st.pending_user_move = false;
                        st.preview = DockEdge::None;
                        let anchor_h = st.pod_h_phys.max(pod_base_h);
                        let (sx, sy, new_edge) =
                            snap_window_rect(wx, wy, ww, anchor_h, live_area, threshold, margin);
                        let seam = has_adjacent_monitor_beyond_edge(
                            &window, sx, sy, ww, anchor_h, live_area, new_edge,
                        );
                        st.edge = new_edge;
                        st.dock_x = sx;
                        st.dock_y = sy;
                        st.work_area = live_area;
                        st.seam_mode = seam;
                        let (ox, oy) = compute_orb_pos_from_pod(
                            sx, sy, ww, anchor_h, orb_size, live_area, new_edge, margin,
                        );
                        st.orb_x = Some(ox);
                        st.orb_y = Some(oy);
                        st.tucked = false;
                        st.orb_collapsed = false;
                        st.idle = false;
                        st.last_hover_at = now;
                        st.grace_until = now + Duration::from_millis(1400);
                        let vis_to =
                            compute_visible_pod_pos(sx, sy, ww, wh, live_area, new_edge, margin);
                        action = WatcherAction::SnapPod {
                            to: vis_to,
                            saved: st.to_saved(),
                            payload: st.payload(),
                        };
                    }
                } else if st.orb_collapsed {
                    if st.play_mode {
                        // 2A. Stress-Relief 2D Gravity Ball Mode!
                        let bounds =
                            OrbPhysicsBounds::from_work_area(st.work_area, orb_size, orb_inset);
                        let dt = now
                            .duration_since(st.last_physics_tick)
                            .as_secs_f64()
                            .clamp(0.001, 0.032);
                        st.last_physics_tick = now;

                        let mut impact = None;
                        if !mouse_down
                            && now.duration_since(st.last_kick_at) >= Duration::from_millis(105)
                        {
                            if let Some((prev_t, prev_cx, prev_cy)) = prev_cursor {
                                let c_dt = now.duration_since(prev_t).as_secs_f64();
                                if let Some(kick_hit) = apply_cursor_kick(
                                    &mut st.physics,
                                    (prev_cx, prev_cy),
                                    (cur.x, cur.y),
                                    c_dt,
                                    orb_size as f64,
                                    bounds,
                                    scale,
                                ) {
                                    st.last_kick_at = now;
                                    impact = Some(kick_hit);
                                }
                            }
                        }

                        if let Some(step_hit) =
                            step_orb_gravity(&mut st.physics, dt, bounds, scale)
                        {
                            if impact.map_or(true, |k| step_hit.normal_speed > k.normal_speed) {
                                impact = Some(step_hit);
                            }
                        }

                        let nx = st.physics.px.round() as i32;
                        let ny = st.physics.py.round() as i32;
                        let pos_changed = nx != wx || ny != wy;
                        let angle_delta = (st.physics.angle_deg - st.last_emitted_angle).abs();
                        let fx = if impact.is_some() || angle_delta >= 2.2 {
                            st.last_emitted_angle = st.physics.angle_deg;
                            Some(OrbFxPayload {
                                angle: st.physics.angle_deg,
                                impact_axis: impact.map(|i| i.axis),
                                impact_speed: impact.map_or(0.0, |i| i.normal_speed),
                                combo: st.physics.combo,
                            })
                        } else {
                            None
                        };

                        if pos_changed {
                            action = WatcherAction::MoveFast {
                                to: (nx, ny),
                                payload: None,
                                fx,
                            };
                        } else if let Some(fx_only) = fx {
                            action = WatcherAction::EmitOrbFx(fx_only);
                        }
                    } else if st.orb_coasting {
                        // 2B. Inertial damped glide when flicked in normal Orb standby
                        let dt = now
                            .duration_since(st.last_physics_tick)
                            .as_secs_f64()
                            .clamp(0.001, 0.032);
                        st.last_physics_tick = now;
                        let min_ox = (st.work_area.x + margin) as f64;
                        let max_ox =
                            ((st.work_area.right() - orb_size - margin) as f64).max(min_ox);
                        let min_oy = (st.work_area.y + margin) as f64;
                        let max_oy =
                            ((st.work_area.bottom() - orb_size - margin) as f64).max(min_oy);

                        let decay = (-7.5 * dt).exp();
                        st.physics.vx *= decay;
                        st.physics.vy *= decay;
                        st.physics.px += st.physics.vx * dt;
                        st.physics.py += st.physics.vy * dt;

                        let mut impact: Option<ImpactEvent> = None;
                        if st.physics.px < min_ox {
                            st.physics.px = min_ox;
                            let s = st.physics.vx.abs() / scale;
                            st.physics.vx = -st.physics.vx * 0.62;
                            if s > 120.0 {
                                impact = Some(ImpactEvent {
                                    axis: "x",
                                    normal_speed: s,
                                });
                            }
                        } else if st.physics.px > max_ox {
                            st.physics.px = max_ox;
                            let s = st.physics.vx.abs() / scale;
                            st.physics.vx = -st.physics.vx * 0.62;
                            if s > 120.0 {
                                impact = Some(ImpactEvent {
                                    axis: "x",
                                    normal_speed: s,
                                });
                            }
                        }
                        if st.physics.py < min_oy {
                            st.physics.py = min_oy;
                            let s = st.physics.vy.abs() / scale;
                            st.physics.vy = -st.physics.vy * 0.62;
                            if s > 120.0 {
                                impact = Some(ImpactEvent {
                                    axis: "y",
                                    normal_speed: s,
                                });
                            }
                        } else if st.physics.py > max_oy {
                            st.physics.py = max_oy;
                            let s = st.physics.vy.abs() / scale;
                            st.physics.vy = -st.physics.vy * 0.62;
                            if s > 120.0 {
                                impact = Some(ImpactEvent {
                                    axis: "y",
                                    normal_speed: s,
                                });
                            }
                        }

                        let nx = st.physics.px.round() as i32;
                        let ny = st.physics.py.round() as i32;
                        if st.physics.vx.hypot(st.physics.vy) < 28.0 * scale {
                            st.orb_coasting = false;
                            let pod_w = st.pod_w_phys.max(pod_default_w);
                            let pod_h = st.pod_h_phys.max(pod_base_h);
                            let (px, py, new_edge) = compute_pod_pos_from_orb(
                                nx, ny, orb_size, pod_w, pod_h, st.work_area, margin,
                            );
                            st.orb_x = Some(nx);
                            st.orb_y = Some(ny);
                            st.dock_x = px;
                            st.dock_y = py;
                            st.edge = new_edge;
                            st.seam_mode = has_adjacent_monitor_beyond_edge(
                                &window, px, py, pod_w, pod_h, st.work_area, new_edge,
                            );
                            st.orb_expand_suppress_until = now + Duration::from_millis(380);
                            action = WatcherAction::SaveOrbDrag {
                                to: (nx, ny),
                                saved: st.to_saved(),
                                payload: st.payload(),
                            };
                        } else {
                            let fx = impact.map(|i| OrbFxPayload {
                                angle: 0.0,
                                impact_axis: Some(i.axis),
                                impact_speed: i.normal_speed,
                                combo: 0,
                            });
                            action = WatcherAction::MoveFast {
                                to: (nx, ny),
                                payload: None,
                                fx,
                            };
                        }
                    } else {
                        // 2C. Hover-to-Expand with 260ms Intent Gate (cancels cleanly on mousedown / drag / dblclick!)
                        let orb_cx = wx as f64 + orb_size as f64 * 0.5;
                        let orb_cy = wy as f64 + orb_size as f64 * 0.5;
                        let hover_r = (ORB_VISUAL_RADIUS_LOGICAL_PX + 2.0) * scale;
                        let dist = (cur.x - orb_cx).hypot(cur.y - orb_cy);
                        let in_sphere = dist <= hover_r;

                        if !in_sphere {
                            st.orb_hover_since = None;
                            if dist > hover_r + 10.0 * scale {
                                st.orb_hover_armed = true;
                            }
                        } else if mouse_down {
                            // Pressing mouse cancels hover-expand so drag & double-click work 100% reliably!
                            st.orb_hover_since = None;
                            st.orb_expand_suppress_until = now + Duration::from_millis(620);
                        } else if st.orb_hover_armed && now >= st.orb_expand_suppress_until {
                            match st.orb_hover_since {
                                None => {
                                    st.orb_hover_since = Some(now);
                                }
                                Some(since) => {
                                    if now.duration_since(since)
                                        >= Duration::from_millis(ORB_HOVER_EXPAND_MS)
                                    {
                                        st.orb_hover_since = None;
                                        st.orb_collapsed = false;
                                        st.idle = false;
                                        st.last_hover_at = now;
                                        st.grace_until = now + Duration::from_millis(1200);
                                        let pod_w = st.pod_w_phys.max(pod_default_w);
                                        let pod_h = st.pod_h_phys.max(pod_base_h);
                                        let to = compute_visible_pod_pos(
                                            st.dock_x,
                                            st.dock_y,
                                            pod_w,
                                            pod_h,
                                            st.work_area,
                                            st.edge,
                                            margin,
                                        );
                                        action = WatcherAction::ExitOrb {
                                            pod_pos: to,
                                            pod_w,
                                            pod_h,
                                            payload: st.payload(),
                                        };
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // Pod is currently expanded — keep strictly anchored to (dock_x, dock_y),
                    // shifting up temporarily ONLY while an open drawer exceeds the bottom work area,
                    // and returning right back to dock_y the instant the drawer closes!
                    let (vis_x, vis_y) = compute_visible_pod_pos(
                        st.dock_x,
                        st.dock_y,
                        ww,
                        wh,
                        st.work_area,
                        st.edge,
                        margin,
                    );
                    if !st.tucked
                        && ((vis_x - wx).abs() > 2 || (vis_y - wy).abs() > 2)
                        && now >= st.ignore_moves_until
                    {
                        action = WatcherAction::AdjustOnScreen {
                            to: (vis_x, vis_y),
                        };
                    } else if st.standby_mode == StandbyMode::Orb {
                        let pad = (8.0 * scale).round() as i32;
                        let inside = is_cursor_in_pod(cx, cy, wx, wy, ww, wh, pad);
                        if inside || st.pinned || st.ui_hold || mouse_down {
                            st.last_hover_at = now;
                            if st.idle {
                                st.idle = false;
                                action = WatcherAction::EmitOnly(st.payload());
                            }
                        } else if now >= st.grace_until
                            && now.duration_since(st.last_hover_at) >= Duration::from_millis(1350)
                        {
                            let pod_w = st.pod_w_phys.max(pod_default_w);
                            let pod_h = st.pod_h_phys.max(pod_base_h);
                            let opos = match (st.orb_x, st.orb_y) {
                                (Some(ox), Some(oy)) => (ox, oy),
                                _ => {
                                    let p = compute_orb_pos_from_pod(
                                        st.dock_x,
                                        st.dock_y,
                                        pod_w,
                                        pod_h,
                                        orb_size,
                                        st.work_area,
                                        st.edge,
                                        margin,
                                    );
                                    st.orb_x = Some(p.0);
                                    st.orb_y = Some(p.1);
                                    p
                                }
                            };
                            action = WatcherAction::EnterOrb {
                                orb_pos: opos,
                                orb_size,
                                play: false,
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
                                        action = WatcherAction::UntuckEdge {
                                            from: (wx, wy),
                                            to,
                                            payload: st.payload(),
                                        };
                                    }
                                } else {
                                    let pad = (8.0 * scale).round() as i32;
                                    let inside = is_cursor_in_pod(cx, cy, wx, wy, ww, wh, pad);
                                    if inside || st.pinned || st.ui_hold || mouse_down {
                                        st.last_hover_at = now;
                                    } else if now >= st.grace_until
                                        && now.duration_since(st.last_hover_at)
                                            >= Duration::from_millis(1200)
                                    {
                                        action = WatcherAction::TuckEdge {
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
            }

            match action {
                WatcherAction::None => {}
                WatcherAction::MoveFast { to, payload, fx } => {
                    set_window_pos_fast(&window, &dock, to.0, to.1);
                    if let Some(p) = payload {
                        let _ = window.emit("dock-state", p);
                    }
                    if let Some(f) = fx {
                        let _ = window.emit("orb-fx", f);
                    }
                }
                WatcherAction::GlideOrbReturn {
                    from,
                    to,
                    payload,
                    fx,
                } => {
                    let _ = window.emit("dock-state", payload);
                    let _ = window.emit("orb-fx", fx);
                    glide_window(&window, &dock, from, to, 10, 12);
                }
                WatcherAction::EmitOnly(payload) => {
                    let _ = window.emit("dock-state", payload);
                }
                WatcherAction::EmitOrbFx(fx) => {
                    let _ = window.emit("orb-fx", fx);
                }
                WatcherAction::AdjustOnScreen { to } => {
                    set_programmatic_pos(&window, &dock, to.0, to.1);
                }
                WatcherAction::SnapPod { to, saved, payload } => {
                    let _ = window.set_ignore_cursor_events(false);
                    if let Ok(cur_pos) = window.outer_position() {
                        if (cur_pos.x, cur_pos.y) != to {
                            glide_window(&window, &dock, (cur_pos.x, cur_pos.y), to, 6, 14);
                        }
                    }
                    persist_saved_dock(&saved);
                    let _ = window.emit("dock-state", payload);
                }
                WatcherAction::SaveOrbDrag { to, saved, payload } => {
                    if let Ok(cur_pos) = window.outer_position() {
                        if (cur_pos.x, cur_pos.y) != to {
                            set_programmatic_pos(&window, &dock, to.0, to.1);
                        }
                    }
                    persist_saved_dock(&saved);
                    let _ = window.emit("dock-state", payload);
                }
                WatcherAction::TuckEdge {
                    edge,
                    dock_x,
                    dock_y,
                    area,
                    seam,
                    peek,
                } => {
                    let _ = window.emit(
                        "collapse-drawers",
                        serde_json::json!({ "immediate": true }),
                    );
                    let f_ww = pod_default_w;
                    let f_wh = pod_base_h;
                    set_window_rect_atomic(&window, &dock, dock_x, dock_y, f_ww, f_wh);
                    let target =
                        tucked_window_pos(dock_x, dock_y, f_ww, f_wh, area, peek, edge, seam);
                    glide_window(&window, &dock, (dock_x, dock_y), target, 10, 14);
                    let payload = {
                        let mut st = dock.lock().unwrap();
                        st.pod_h_phys = pod_base_h;
                        st.tucked = true;
                        st.idle = true;
                        st.payload()
                    };
                    let _ = window.set_ignore_cursor_events(true);
                    let _ = window.emit("dock-state", payload);
                }
                WatcherAction::UntuckEdge { from, to, payload } => {
                    let _ = window.set_ignore_cursor_events(false);
                    let _ = window.emit("dock-state", payload);
                    glide_window(&window, &dock, from, to, 10, 14);
                }
                WatcherAction::EnterOrb {
                    orb_pos,
                    orb_size,
                    play,
                } => {
                    let _ = window.emit(
                        "collapse-drawers",
                        serde_json::json!({ "immediate": true }),
                    );
                    let payload = {
                        let mut st = dock.lock().unwrap();
                        st.pod_h_phys = pod_base_h;
                        st.orb_collapsed = true;
                        st.play_mode = play;
                        st.tucked = false;
                        st.idle = true;
                        st.orb_hover_since = None;
                        st.orb_hover_armed = false;
                        st.orb_expand_suppress_until = Instant::now() + Duration::from_millis(480);
                        st.ignore_moves_until = Instant::now() + Duration::from_millis(320);
                        st.payload()
                    };
                    let _ = window.set_ignore_cursor_events(false);
                    let _ = window.emit("dock-state", payload);
                    if !play {
                        let _ = window.emit(
                            "orb-fx",
                            OrbFxPayload {
                                angle: 0.0,
                                impact_axis: None,
                                impact_speed: 0.0,
                                combo: 0,
                            },
                        );
                    }
                    std::thread::sleep(Duration::from_millis(16));
                    set_window_rect_atomic(
                        &window,
                        &dock,
                        orb_pos.0,
                        orb_pos.1,
                        orb_size,
                        orb_size,
                    );
                }
                WatcherAction::ExitOrb {
                    pod_pos,
                    pod_w,
                    pod_h,
                    payload,
                } => {
                    let _ = window.set_ignore_cursor_events(false);
                    let _ = window.emit(
                        "orb-fx",
                        OrbFxPayload {
                            angle: 0.0,
                            impact_axis: None,
                            impact_speed: 0.0,
                            combo: 0,
                        },
                    );
                    set_window_rect_atomic(&window, &dock, pod_pos.0, pod_pos.1, pod_w, pod_h);
                    let _ = window.emit("dock-state", payload);
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
fn set_pod_envelope(
    window: WebviewWindow,
    dock: State<'_, DockController>,
    logical_h: f64,
    collapsed: bool,
) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let clamped_logical = logical_h.clamp(POD_BASE_HEIGHT_LOGICAL_PX, 960.0);
    let new_h = (clamped_logical * scale).round() as i32;
    let base_h = (POD_BASE_HEIGHT_LOGICAL_PX * scale).round() as i32;
    let default_w = (POD_WIDTH_LOGICAL_PX * scale).round() as i32;
    let margin = (8.0 * scale).round() as i32;

    let rect_opt = {
        let mut st = dock.inner.lock().unwrap();
        if st.tucked || st.orb_collapsed {
            None
        } else {
            let pod_w = st.pod_w_phys.max(default_w);
            if collapsed {
                st.pod_h_phys = base_h;
            }
            let (vis_x, vis_y) = compute_visible_pod_pos(
                st.dock_x,
                st.dock_y,
                pod_w,
                new_h,
                st.work_area,
                st.edge,
                margin,
            );
            Some((vis_x, vis_y, pod_w, new_h))
        }
    };

    if let Some((vx, vy, vw, vh)) = rect_opt {
        set_window_rect_atomic(&window, &dock.inner, vx, vy, vw, vh);
    }
}

#[tauri::command]
fn set_dock_pinned(app: AppHandle, dock: State<'_, DockController>, pinned: bool) -> DockStatePayload {
    let (was_collapsed, payload) = {
        let mut st = dock.inner.lock().unwrap();
        let prev_collapsed = st.tucked || st.orb_collapsed;
        st.pinned = pinned;
        st.last_hover_at = Instant::now();
        if pinned && prev_collapsed {
            st.force_untuck = true;
        } else if pinned {
            st.idle = false;
        }
        (prev_collapsed, st.payload())
    };
    if !was_collapsed {
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
fn set_standby_mode(
    app: AppHandle,
    dock: State<'_, DockController>,
    mode: String,
) -> Result<DockStatePayload, String> {
    let parsed = match mode.trim().to_ascii_lowercase().as_str() {
        "edge" => StandbyMode::Edge,
        "orb" => StandbyMode::Orb,
        _ => return Err("待机模式仅支持 edge 或 orb".to_string()),
    };
    let (was_collapsed, saved, payload) = {
        let mut st = dock.inner.lock().unwrap();
        let prev_collapsed = st.tucked || st.orb_collapsed;
        st.standby_mode = parsed;
        let now = Instant::now();
        st.last_hover_at = now;
        st.grace_until = now + Duration::from_millis(2400);
        if prev_collapsed {
            st.force_untuck = true;
        } else {
            st.idle = false;
        }
        (prev_collapsed, st.to_saved(), st.payload())
    };
    persist_saved_dock(&saved);
    if !was_collapsed {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.emit("dock-state", payload.clone());
        }
    }
    Ok(payload)
}

#[tauri::command]
fn expand_from_orb(dock: State<'_, DockController>) {
    let mut st = dock.inner.lock().unwrap();
    if st.orb_collapsed || st.tucked {
        st.play_mode = false;
        st.orb_coasting = false;
        st.force_untuck = true;
    }
}

#[tauri::command]
fn begin_window_drag(window: WebviewWindow, dock: State<'_, DockController>) {
    let (Ok(cur), Ok(wpos), Ok(wsize)) = (
        window.cursor_position(),
        window.outer_position(),
        window.outer_size(),
    ) else {
        return;
    };
    let cx = cur.x.round() as i32;
    let cy = cur.y.round() as i32;
    let ww = (wsize.width as i32).max(1);
    let wh = (wsize.height as i32).max(1);
    let off_x = (cx - wpos.x).clamp(0, ww);
    let off_y = (cy - wpos.y).clamp(0, wh);
    let now = Instant::now();

    let mut st = dock.inner.lock().unwrap();
    let t_sec = now.duration_since(st.epoch).as_secs_f64();
    st.custom_dragging = true;
    st.orb_coasting = false;
    st.drag_offset = (off_x, off_y);
    st.vel_tracker.clear();
    st.vel_tracker.push(t_sec, wpos.x as f64, wpos.y as f64);
    st.physics.vx = 0.0;
    st.physics.vy = 0.0;
    st.physics.omega = 0.0;
    st.physics.sleeping = true;
    st.orb_hover_since = None;
    st.orb_hover_armed = false;
    st.orb_expand_suppress_until = now + Duration::from_millis(650);
    st.last_hover_at = now;
}

#[tauri::command]
fn toggle_orb_play_mode(
    window: WebviewWindow,
    dock: State<'_, DockController>,
) -> DockStatePayload {
    let scale = window.scale_factor().unwrap_or(1.0);
    let orb_size = (ORB_SIZE_LOGICAL_PX * scale).round() as i32;
    let margin = (8.0 * scale).round() as i32;
    let pod_default_w = (POD_WIDTH_LOGICAL_PX * scale).round() as i32;
    let pod_base_h = (POD_BASE_HEIGHT_LOGICAL_PX * scale).round() as i32;
    let cur_pos = window.outer_position().ok().map(|p| (p.x, p.y));
    let cur_cursor = window.cursor_position().ok().map(|c| (c.x.round() as i32, c.y.round() as i32));
    let now = Instant::now();

    let mut emit_now = false;
    let payload = {
        let mut st = dock.inner.lock().unwrap();
        st.custom_dragging = false;
        st.orb_coasting = false;
        st.orb_hover_since = None;
        st.orb_hover_armed = false;
        st.orb_expand_suppress_until = now + Duration::from_millis(650);

        if st.play_mode {
            // Exit Gravity Play Mode
            st.play_mode = false;
            let prev_standby = st.pre_play_standby.take().unwrap_or(st.standby_mode);
            st.standby_mode = prev_standby;
            let home_orb = st.pre_play_orb_pos.take().unwrap_or_else(|| {
                let pod_w = st.pod_w_phys.max(pod_default_w);
                let pod_h = st.pod_h_phys.max(pod_base_h);
                compute_orb_pos_from_pod(
                    st.dock_x,
                    st.dock_y,
                    pod_w,
                    pod_h,
                    orb_size,
                    st.work_area,
                    st.edge,
                    margin,
                )
            });

            if prev_standby == StandbyMode::Orb && st.orb_collapsed {
                st.orb_x = Some(home_orb.0);
                st.orb_y = Some(home_orb.1);
                st.pending_orb_return = Some(home_orb);
            } else {
                st.physics = OrbPhysics::default();
                st.force_untuck = true;
            }
        } else {
            // Enter Gravity Play Mode!
            let start_pos = if st.orb_collapsed {
                let p = cur_pos.unwrap_or_else(|| {
                    (
                        st.orb_x.unwrap_or(st.work_area.x + margin),
                        st.orb_y.unwrap_or(st.work_area.y + margin),
                    )
                });
                st.pre_play_orb_pos = Some((st.orb_x.unwrap_or(p.0), st.orb_y.unwrap_or(p.1)));
                st.pre_play_standby = None;
                emit_now = true;
                p
            } else {
                let pod_w = st.pod_w_phys.max(pod_default_w);
                let pod_h = st.pod_h_phys.max(pod_base_h);
                let home = compute_orb_pos_from_pod(
                    st.dock_x,
                    st.dock_y,
                    pod_w,
                    pod_h,
                    orb_size,
                    st.work_area,
                    st.edge,
                    margin,
                );
                st.pre_play_orb_pos = Some(home);
                st.pre_play_standby = Some(st.standby_mode);
                st.force_tuck = true;
                if let Some((cx, cy)) = cur_cursor {
                    let min_x = st.work_area.x + margin;
                    let max_x = (st.work_area.right() - orb_size - margin).max(min_x);
                    let min_y = st.work_area.y + margin;
                    let max_y = (st.work_area.bottom() - orb_size - margin).max(min_y);
                    (
                        (cx - orb_size / 2).clamp(min_x, max_x),
                        (cy - orb_size / 2).clamp(min_y, max_y),
                    )
                } else {
                    home
                }
            };

            let live_area = monitor_work_rect(&window, start_pos.0 + orb_size / 2, start_pos.1 + orb_size / 2)
                .unwrap_or(st.work_area);
            st.work_area = live_area;
            st.play_mode = true;
            st.physics = OrbPhysics {
                px: start_pos.0 as f64,
                py: start_pos.1 as f64,
                vx: 165.0 * scale,
                vy: -430.0 * scale,
                omega: 4.2,
                angle_deg: 0.0,
                sleeping: false,
                combo: 0,
            };
            st.last_physics_tick = now;
        }
        st.payload()
    };

    if emit_now {
        let _ = window.emit("dock-state", payload.clone());
    }
    payload
}

#[tauri::command]
fn orb_tap_impulse(window: WebviewWindow, dock: State<'_, DockController>) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let orb_size = (ORB_SIZE_LOGICAL_PX * scale).round() as f64;
    let r = (ORB_VISUAL_RADIUS_LOGICAL_PX * scale).max(16.0);
    let cur_opt = window.cursor_position().ok();
    let now = Instant::now();

    let fx_opt = {
        let mut st = dock.inner.lock().unwrap();
        if !st.orb_collapsed || !st.play_mode {
            None
        } else {
            let lateral = if let Some(cur) = cur_opt {
                let bcx = st.physics.px + orb_size * 0.5;
                let norm = ((bcx - cur.x) / r).clamp(-1.0, 1.0);
                norm * 460.0 * scale
            } else {
                0.0
            };
            st.physics.vx = (st.physics.vx * 0.45 + lateral).clamp(-2600.0 * scale, 2600.0 * scale);
            st.physics.vy = (-940.0 * scale).min(st.physics.vy - 520.0 * scale);
            st.physics.omega = (st.physics.vx / r) * 0.9;
            st.physics.sleeping = false;
            st.physics.combo = st.physics.combo.saturating_add(1);
            st.last_physics_tick = now;
            Some(OrbFxPayload {
                angle: st.physics.angle_deg,
                impact_axis: Some("y"),
                impact_speed: 920.0,
                combo: st.physics.combo,
            })
        }
    };

    if let Some(fx) = fx_opt {
        let _ = window.emit("orb-fx", fx);
    }
}

#[tauri::command]
fn suppress_orb_hover(dock: State<'_, DockController>, ms: u64) {
    let mut st = dock.inner.lock().unwrap();
    let clamped = ms.clamp(80, 2000);
    st.orb_hover_since = None;
    st.orb_expand_suppress_until = Instant::now() + Duration::from_millis(clamped);
}

#[tauri::command]
fn tuck_or_hide(app: AppHandle, dock: State<'_, DockController>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let should_hide = {
        let mut st = dock.inner.lock().unwrap();
        if st.orb_collapsed && st.play_mode {
            // Pressing Escape while in Gravity Play Mode exits play mode back to parked Orb / Pod!
            let prev_standby = st.pre_play_standby.take().unwrap_or(st.standby_mode);
            st.standby_mode = prev_standby;
            if prev_standby == StandbyMode::Orb {
                if let Some(home) = st.pre_play_orb_pos.take() {
                    st.orb_x = Some(home.0);
                    st.orb_y = Some(home.1);
                    st.pending_orb_return = Some(home);
                } else {
                    st.play_mode = false;
                    st.physics = OrbPhysics::default();
                }
            } else {
                st.play_mode = false;
                st.physics = OrbPhysics::default();
                st.force_untuck = true;
            }
            false
        } else if st.standby_mode == StandbyMode::Orb || st.edge != DockEdge::None {
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
            if st.tucked || st.orb_collapsed {
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
    cleanup_stale_old_exe();
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
            check_app_update,
            apply_app_update,
            get_dock_state,
            set_dock_hold,
            set_pod_envelope,
            set_dock_pinned,
            set_standby_mode,
            expand_from_orb,
            begin_window_drag,
            toggle_orb_play_mode,
            orb_tap_impulse,
            suppress_orb_hover,
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
        // Dragged near top -> snaps flush to Top (y = 0) and tucks upward leaving 9px peek
        assert_eq!(
            snap_window_rect(1040, 26, 480, 64, area, 48, 8),
            (1040, 0, DockEdge::Top)
        );
        assert_eq!(
            tucked_window_pos(1040, 0, 480, 64, area, 9, DockEdge::Top, false),
            (1040, -55)
        );

        // Dragged near right edge -> snaps flush to Right (x = 2560 - 480 = 2080) and tucks rightward to x = 2551
        assert_eq!(
            snap_window_rect(2065, 520, 480, 64, area, 48, 8),
            (2080, 520, DockEdge::Right)
        );
        assert_eq!(
            tucked_window_pos(2080, 520, 480, 64, area, 9, DockEdge::Right, false),
            (2551, 520)
        );

        // Dragged near left edge -> snaps flush to Left (x = 0) and tucks leftward to x = -471
        assert_eq!(
            snap_window_rect(18, 480, 480, 64, area, 48, 8),
            (0, 480, DockEdge::Left)
        );
        assert_eq!(
            tucked_window_pos(0, 480, 480, 64, area, 9, DockEdge::Left, false),
            (-471, 480)
        );

        // Dragged to center -> stays free-floating (DockEdge::None)
        assert_eq!(
            snap_window_rect(800, 450, 480, 64, area, 48, 8),
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
        // Top docked at (1040, 0), width 480 -> center_x = 1280
        assert!(is_cursor_in_tucked_handle(
            1280, 2, 1040, 0, 480, 64, area, 1.0, DockEdge::Top
        ));
        assert!(!is_cursor_in_tucked_handle(
            2400, 2, 1040, 0, 480, 64, area, 1.0, DockEdge::Top
        ));
        assert!(!is_cursor_in_tucked_handle(
            1280, 35, 1040, 0, 480, 64, area, 1.0, DockEdge::Top
        ));

        // Left docked at (0, 500), height 64 -> center_y = 532, x in [-4..14]
        assert!(is_cursor_in_tucked_handle(
            3, 530, 0, 500, 480, 64, area, 1.0, DockEdge::Left
        ));
        assert!(!is_cursor_in_tucked_handle(
            3, 900, 0, 500, 480, 64, area, 1.0, DockEdge::Left
        ));

        // Right docked at (2080, 500), height 64 -> center_y = 532, x in [2546..2564]
        assert!(is_cursor_in_tucked_handle(
            2557, 530, 2080, 500, 480, 64, area, 1.0, DockEdge::Right
        ));
        assert!(!is_cursor_in_tucked_handle(
            2557, 200, 2080, 500, 480, 64, area, 1.0, DockEdge::Right
        ));
    }

    #[test]
    fn drawer_expand_and_collapse_returns_to_exact_home_y() {
        let area = WorkRect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        };
        let home_x = 1800;
        let home_y = 1280;
        // Collapsed single-row height (64px): stays at exact home_y = 1280
        assert_eq!(
            compute_visible_pod_pos(home_x, home_y, 480, 64, area, DockEdge::None, 8),
            (1800, 1280)
        );
        // Expanded platter height (380px): temporarily pushed up to 1400 - 380 - 8 = 1012
        assert_eq!(
            compute_visible_pod_pos(home_x, home_y, 480, 380, area, DockEdge::None, 8),
            (1800, 1012)
        );
        // Closing platter back to 64px: returns pixel-perfectly to original home_y = 1280!
        assert_eq!(
            compute_visible_pod_pos(home_x, home_y, 480, 64, area, DockEdge::None, 8),
            (1800, 1280)
        );
    }

    #[test]
    fn shield_orb_and_pod_anchor_roundtrip() {
        let area = WorkRect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        };
        // Pod docked at Right edge (2080, 600) -> 60x60 Orb parks near right edge at (2560 - 60 - 8 = 2492, 602)
        let (ox, oy) = compute_orb_pos_from_pod(2080, 600, 480, 64, 60, area, DockEdge::Right, 8);
        assert_eq!((ox, oy), (2492, 602));

        // Dragging the Orb to right edge (2492, 800) updates pod home to Right-docked (2080, 798)
        let (px, py, edge) = compute_pod_pos_from_orb(2492, 800, 60, 480, 64, area, 8);
        assert_eq!((px, py, edge), (2080, 798, DockEdge::Right));

        // Dragging the Orb to middle of screen (1200, 500) updates pod home to Free-floating centered on the orb
        let (px2, py2, edge2) = compute_pod_pos_from_orb(1200, 500, 60, 480, 64, area, 8);
        assert_eq!((px2, py2, edge2), (990, 498, DockEdge::None));
    }

    #[test]
    fn hot_update_sibling_path_and_build_git_meta() {
        let p = PathBuf::from(r"C:\Users\demo\.claude-switcher\envguard\claude.envguard.exe");
        assert_eq!(
            sibling_path_with_suffix(&p, ".old"),
            PathBuf::from(r"C:\Users\demo\.claude-switcher\envguard\claude.envguard.exe.old")
        );
        let sha = env!("ENVGUARD_GIT_SHA");
        let short_sha = env!("ENVGUARD_GIT_SHORT_SHA");
        assert!(sha == "unknown" || sha.len() == 40);
        assert!(short_sha == "unknown" || short_sha.len() == 7);
    }

    #[test]
    fn gravity_orb_bounces_off_floor_walls_and_sleeps_cleanly() {
        let area = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        };
        let bounds = OrbPhysicsBounds::from_work_area(area, 72, 8);
        // Visual 56px sphere inside 72px stage touches taskbar y=1040 cleanly at py = 1040 - 72 + 8 = 976
        assert_eq!(bounds.max_y, 976.0);
        assert_eq!(bounds.min_x, -8.0);
        assert_eq!(bounds.max_x, 1856.0);

        let mut phys = OrbPhysics {
            px: 400.0,
            py: 970.0,
            vx: 220.0,
            vy: 900.0,
            omega: 0.0,
            angle_deg: 0.0,
            sleeping: false,
            combo: 3,
        };

        // First step crosses floor -> rebounds upward (vy < 0), resets juggle combo, emits Y impact
        let hit = step_orb_gravity(&mut phys, 0.016, bounds, 1.0);
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().axis, "y");
        assert!(phys.vy < -400.0);
        assert_eq!(phys.combo, 0);
        assert!(!phys.sleeping);

        // Simulate 8 seconds of settling -> ball must come to complete 0% CPU sleep on the taskbar floor
        for _ in 0..1000 {
            if phys.sleeping {
                break;
            }
            step_orb_gravity(&mut phys, 0.008, bounds, 1.0);
        }
        assert!(phys.sleeping, "gravity orb must enter sleep state once settled");
        assert_eq!(phys.py, bounds.max_y);
        assert_eq!(phys.vx, 0.0);
        assert_eq!(phys.vy, 0.0);
    }

    #[test]
    fn cursor_sweep_kicks_ball_upward_and_increments_combo_without_resting_jitter() {
        let area = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        };
        let bounds = OrbPhysicsBounds::from_work_area(area, 72, 8);
        let mut phys = OrbPhysics {
            px: 600.0,
            py: 500.0,
            vx: 0.0,
            vy: 300.0,
            omega: 0.0,
            angle_deg: 0.0,
            sleeping: false,
            combo: 0,
        };

        // Cursor already resting inside the ball (636, 536) wiggling slightly -> must NOT kick
        let no_kick = apply_cursor_kick(&mut phys, (634.0, 536.0), (638.0, 534.0), 0.016, 72.0, bounds, 1.0);
        assert!(no_kick.is_none());
        assert_eq!(phys.combo, 0);

        // Fast upward swipe from below-outside (636, 590) -> through ball (636, 528) in 16ms -> kicks upward!
        let kick = apply_cursor_kick(&mut phys, (636.0, 590.0), (636.0, 528.0), 0.016, 72.0, bounds, 1.0);
        assert!(kick.is_some());
        assert!(phys.vy < -400.0, "cursor kick must launch orb upward");
        assert_eq!(phys.combo, 1);
    }

    #[test]
    fn velocity_tracker_computes_clamped_flick_vector() {
        let mut vt = VelocityTracker::default();
        vt.push(0.000, 100.0, 200.0);
        vt.push(0.016, 132.0, 168.0);
        vt.push(0.032, 164.0, 136.0);
        let (vx, vy) = vt.velocity(4000.0);
        assert!((vx - 2000.0).abs() < 1e-3);
        assert!((vy + 2000.0).abs() < 1e-3);

        // Extreme spike gets clamped cleanly to max_speed
        vt.clear();
        vt.push(0.000, 0.0, 0.0);
        vt.push(0.010, 900.0, 0.0);
        let (cvx, _) = vt.velocity(3000.0);
        assert!((cvx - 3000.0).abs() < 1e-3);
    }
}
