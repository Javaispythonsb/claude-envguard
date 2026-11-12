#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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
    charge: bool,
}

impl OrbFxPayload {
    fn reset() -> Self {
        Self {
            angle: 0.0,
            impact_axis: None,
            impact_speed: 0.0,
            combo: 0,
            charge: false,
        }
    }

    fn charge(angle: f64, combo: u32) -> Self {
        Self {
            angle,
            impact_axis: None,
            impact_speed: 0.0,
            combo,
            charge: true,
        }
    }

    fn impact(angle: f64, combo: u32, hit: Option<ImpactEvent>) -> Self {
        Self {
            angle,
            impact_axis: hit.map(|h| h.axis),
            impact_speed: hit.map_or(0.0, |h| h.normal_speed),
            combo,
            charge: false,
        }
    }
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

/// A primary-button press that began on the ball. The watcher thread owns the rest of the
/// gesture: it decides between tap, double press, hold and drag by polling the button.
#[derive(Clone, Copy, Debug)]
struct OrbPress {
    started: Instant,
    press_cursor: (i32, i32),
    grab_offset: (i32, i32),
    moved: bool,
}

struct DockInner {
    epoch: Instant,
    hwnd: isize,
    scale: f64,
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
    orb_press: Option<OrbPress>,
    press_consumed: bool,
    pending_tap: Option<Instant>,
    toggle_lock_until: Instant,
    kick_block_until: Instant,
    last_play_interaction: Instant,
    play_exited_at: Option<Instant>,
    escape_was_down: bool,
    orb_hover_armed: bool,
    orb_hover_since: Option<Instant>,
    orb_expand_suppress_until: Instant,
    pre_play_orb_pos: Option<(i32, i32)>,
    pre_play_standby: Option<StandbyMode>,
    pre_play_work_area: Option<WorkRect>,
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
            hwnd: 0,
            scale: 1.0,
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
            orb_press: None,
            press_consumed: false,
            pending_tap: None,
            toggle_lock_until: now,
            kick_block_until: now,
            last_play_interaction: now,
            play_exited_at: None,
            escape_was_down: false,
            orb_hover_armed: true,
            orb_hover_since: None,
            orb_expand_suppress_until: now,
            pre_play_orb_pos: None,
            pre_play_standby: None,
            pre_play_work_area: None,
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
    waker: Arc<OnceLock<std::thread::Thread>>,
}

impl DockController {
    /// Wakes the watcher thread right away instead of waiting for its next poll.
    fn wake(&self) {
        if let Some(thread) = self.waker.get() {
            thread.unpark();
        }
    }
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

/// Splits one line of script output into a progress report, if it is one.
/// The script writes `PROGRESS:<stage>:<percent>` lines while it works.
fn parse_progress_line(line: &str) -> Option<(String, u32)> {
    let rest = line.trim().strip_prefix("PROGRESS:")?;
    let (stage, pct) = rest.rsplit_once(':')?;
    if stage.is_empty() || !stage.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let pct: u32 = pct.trim().parse().ok()?;
    Some((stage.to_string(), pct.min(100)))
}

/// Like `execute_core_blocking`, but reads stdout line by line while the script runs and
/// hands progress lines to `on_progress` instead of keeping them in the output.
fn execute_core_streaming(
    script: PathBuf,
    args: Vec<String>,
    app_version: String,
    mut on_progress: impl FnMut(&str, u32),
) -> Result<CommandOutcome, String> {
    use std::io::{BufRead, BufReader, Read};

    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .args(&args)
        .env("ENVGUARD_APP_VERSION", app_version)
        .env("ENVGUARD_APP_COMMIT", env!("ENVGUARD_GIT_SHA"))
        .env("ENVGUARD_APP_COMMIT_UNIX", env!("ENVGUARD_GIT_COMMIT_UNIX"))
        .env("ENVGUARD_PROGRESS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 PowerShell 核心引擎: {e}"))?;

    // stderr is drained on its own thread so a full stderr pipe can never stall the script
    // while this thread is blocked reading stdout.
    let stderr_reader = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            buf
        })
    });

    let mut kept = Vec::new();
    if let Some(out) = child.stdout.take() {
        let mut reader = BufReader::new(out);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line);
                    if let Some((stage, pct)) = parse_progress_line(&text) {
                        on_progress(&stage, pct);
                    } else {
                        kept.extend_from_slice(&line);
                    }
                }
            }
        }
    }

    let status = child
        .wait()
        .map_err(|e| format!("等待核心引擎返回失败: {e}"))?;
    let err_bytes = stderr_reader
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    let stdout = String::from_utf8_lossy(&kept);
    let stderr = String::from_utf8_lossy(&err_bytes);
    let combined = if stderr.trim().is_empty() {
        stdout.trim().to_string()
    } else if stdout.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        format!("{}\n{}", stdout.trim(), stderr.trim())
    };
    Ok(CommandOutcome {
        output: combined,
        exit_code: status.code().unwrap_or(1),
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

    let script = resolve_core_script(&app)?;
    let app_version = app.package_info().version.to_string();
    let args = vec![
        "stage-app-update".to_string(),
        exe_dir.to_string_lossy().into_owned(),
    ];
    let progress_app = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        execute_core_streaming(script, args, app_version, |stage, pct| {
            let _ = progress_app.emit(
                "app-update-progress",
                serde_json::json!({ "stage": stage, "percent": pct }),
            );
        })
    })
    .await
    .map_err(|e| format!("后台任务执行异常: {e}"))??;

    if outcome.exit_code != 0 {
        return Ok(outcome);
    }
    let _ = app.emit(
        "app-update-progress",
        serde_json::json!({ "stage": "swap", "percent": 98 }),
    );

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
    let _ = app.emit(
        "app-update-progress",
        serde_json::json!({ "stage": "restart", "percent": 100 }),
    );

    let quitting_flag = Arc::clone(&guard.quitting);
    let app_for_exit = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(220));
        quitting_flag.store(true, Ordering::SeqCst);
        app_for_exit.exit(0);
    });

    Ok(outcome)
}

/// Direct Win32 reads and moves. The watcher thread ticks every few milliseconds and often
/// holds the dock mutex, so it must not go through Tauri window getters: those round-trip
/// to the UI thread, and the UI thread takes the same mutex in the Moved handler and in
/// commands, which can deadlock.
#[cfg(target_os = "windows")]
mod native {
    use super::WorkRect;
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct MonitorInfo {
        cb_size: u32,
        rc_monitor: Rect,
        rc_work: Rect,
        dw_flags: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetCursorPos(point: *mut Point) -> i32;
        fn GetWindowRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
        fn IsWindowVisible(hwnd: *mut c_void) -> i32;
        fn MonitorFromPoint(point: Point, flags: u32) -> *mut c_void;
        fn GetMonitorInfoW(monitor: *mut c_void, info: *mut MonitorInfo) -> i32;
        fn SetWindowPos(
            hwnd: *mut c_void,
            insert_after: *mut c_void,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
        fn GetAsyncKeyState(vkey: i32) -> i16;
        fn GetSystemMetrics(index: i32) -> i32;
        fn GetDoubleClickTime() -> u32;
    }

    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const MONITOR_DEFAULTTONULL: u32 = 0;
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    const VK_LBUTTON: i32 = 0x01;
    const VK_RBUTTON: i32 = 0x02;
    const VK_ESCAPE: i32 = 0x1B;
    const SM_SWAPBUTTON: i32 = 23;

    pub fn cursor_pos() -> Option<(i32, i32)> {
        let mut p = Point::default();
        // SAFETY: GetCursorPos only writes into the POINT we pass.
        if unsafe { GetCursorPos(&mut p) } != 0 {
            Some((p.x, p.y))
        } else {
            None
        }
    }

    pub fn window_rect(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
        if hwnd == 0 {
            return None;
        }
        let mut r = Rect::default();
        // SAFETY: GetWindowRect only writes into the RECT we pass; a stale handle just fails.
        if unsafe { GetWindowRect(hwnd as *mut c_void, &mut r) } != 0 {
            Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
        } else {
            None
        }
    }

    pub fn is_visible(hwnd: isize) -> Option<bool> {
        if hwnd == 0 {
            return None;
        }
        // SAFETY: Stateless query on a window handle.
        Some(unsafe { IsWindowVisible(hwnd as *mut c_void) } != 0)
    }

    pub fn work_rect_at(x: i32, y: i32) -> Option<WorkRect> {
        // SAFETY: MonitorFromPoint and GetMonitorInfoW are thread-safe and only write into
        // the MONITORINFO we pass, whose cbSize is set.
        unsafe {
            let monitor = MonitorFromPoint(Point { x, y }, MONITOR_DEFAULTTONEAREST);
            if monitor.is_null() {
                return None;
            }
            let mut info = MonitorInfo {
                cb_size: std::mem::size_of::<MonitorInfo>() as u32,
                rc_monitor: Rect::default(),
                rc_work: Rect::default(),
                dw_flags: 0,
            };
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return None;
            }
            let w = info.rc_work;
            Some(WorkRect {
                x: w.left,
                y: w.top,
                w: w.right - w.left,
                h: w.bottom - w.top,
            })
        }
    }

    pub fn monitor_exists_at(x: i32, y: i32) -> bool {
        // SAFETY: Stateless query.
        !unsafe { MonitorFromPoint(Point { x, y }, MONITOR_DEFAULTTONULL) }.is_null()
    }

    /// Moves the window without resizing, re-ordering or activating it. Because the window
    /// itself is moved every frame, Windows never draws its drag outline, whatever the
    /// "show window contents while dragging" setting is.
    pub fn set_pos(hwnd: isize, x: i32, y: i32) -> bool {
        if hwnd == 0 {
            return false;
        }
        // SAFETY: SetWindowPos on our own top-level window.
        unsafe {
            SetWindowPos(
                hwnd as *mut c_void,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            ) != 0
        }
    }

    /// Sets position and size in one call so both land in the same frame.
    pub fn set_rect(hwnd: isize, x: i32, y: i32, w: i32, h: i32) -> bool {
        if hwnd == 0 {
            return false;
        }
        // SAFETY: SetWindowPos on our own top-level window.
        unsafe {
            SetWindowPos(
                hwnd as *mut c_void,
                std::ptr::null_mut(),
                x,
                y,
                w.max(1),
                h.max(1),
                SWP_NOZORDER | SWP_NOACTIVATE,
            ) != 0
        }
    }

    fn key_down(vkey: i32) -> bool {
        // SAFETY: Stateless query.
        (unsafe { GetAsyncKeyState(vkey) } as u16 & 0x8000) != 0
    }

    pub fn primary_button_down() -> bool {
        // SAFETY: Stateless query.
        let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
        key_down(if swapped { VK_RBUTTON } else { VK_LBUTTON })
    }

    pub fn escape_down() -> bool {
        key_down(VK_ESCAPE)
    }

    pub fn double_click_ms() -> u32 {
        // SAFETY: Stateless query.
        unsafe { GetDoubleClickTime() }
    }
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
mod native {
    use super::WorkRect;

    pub fn cursor_pos() -> Option<(i32, i32)> {
        None
    }
    pub fn window_rect(_hwnd: isize) -> Option<(i32, i32, i32, i32)> {
        None
    }
    pub fn is_visible(_hwnd: isize) -> Option<bool> {
        None
    }
    pub fn work_rect_at(_x: i32, _y: i32) -> Option<WorkRect> {
        None
    }
    pub fn monitor_exists_at(_x: i32, _y: i32) -> bool {
        false
    }
    pub fn set_pos(_hwnd: isize, _x: i32, _y: i32) -> bool {
        false
    }
    pub fn set_rect(_hwnd: isize, _x: i32, _y: i32, _w: i32, _h: i32) -> bool {
        false
    }
    pub fn primary_button_down() -> bool {
        false
    }
    pub fn escape_down() -> bool {
        false
    }
    pub fn double_click_ms() -> u32 {
        500
    }
}

fn is_primary_mouse_down() -> bool {
    native::primary_button_down()
}

const PEEK_LOGICAL_PX: f64 = 9.0;
const SNAP_THRESHOLD_LOGICAL_PX: f64 = 48.0;
const POD_WIDTH_LOGICAL_PX: f64 = 480.0;
const POD_BASE_HEIGHT_LOGICAL_PX: f64 = 64.0;
const ORB_SIZE_LOGICAL_PX: f64 = 72.0;
const ORB_VISUAL_RADIUS_LOGICAL_PX: f64 = 28.0;
const ORB_INSET_LOGICAL_PX: f64 = 8.0;

/// Hovering has to last this long, with the cursor not sweeping through, before the ball
/// opens into the capsule. Long enough that someone who came to press or double-click the
/// ball normally gets there first.
const ORB_HOVER_EXPAND_MS: u64 = 380;
const ORB_HOVER_MAX_SPEED_LOGICAL: f64 = 520.0;
const ORB_DRAG_SLOP_LOGICAL_PX: f64 = 5.0;
const ORB_TAP_MAX_MS: u64 = 300;
const ORB_TOGGLE_LOCK_MS: u64 = 350;
const ORB_KICK_BLOCK_AFTER_PRESS_MS: u64 = 400;
/// A dropped ball only keeps sliding after a deliberate flick, never after a normal placement.
const ORB_COAST_MIN_SPEED_LOGICAL: f64 = 1100.0;
/// Cursor speeds needed to kick the ball. Ordinary pointing arrives well below these, so
/// reaching for the ball to grab or close it does not knock it away.
const ORB_KICK_MIN_SPEED_REST_LOGICAL: f64 = 1400.0;
const ORB_KICK_MIN_SPEED_AIR_LOGICAL: f64 = 900.0;
const ORB_PLAY_IDLE_RETURN_SECS: u64 = 45;
const ORB_PLAY_IDLE_AFTER_KICK_SECS: u64 = 6;
const ORB_PRESS_MAX_IPC_LAG_MS: f64 = 350.0;

/// Pixel sizes derived from the current DPI scale.
#[derive(Clone, Copy, Debug)]
struct DockMetrics {
    scale: f64,
    peek: i32,
    threshold: i32,
    margin: i32,
    orb_size: i32,
    orb_inset: i32,
    orb_radius: f64,
    pod_w: i32,
    pod_h: i32,
}

impl DockMetrics {
    fn new(scale: f64) -> Self {
        let scale = if scale.is_finite() && scale > 0.2 {
            scale
        } else {
            1.0
        };
        let px = |logical: f64| (logical * scale).round() as i32;
        Self {
            scale,
            peek: px(PEEK_LOGICAL_PX),
            threshold: px(SNAP_THRESHOLD_LOGICAL_PX),
            margin: px(8.0),
            orb_size: px(ORB_SIZE_LOGICAL_PX),
            orb_inset: px(ORB_INSET_LOGICAL_PX),
            orb_radius: (ORB_VISUAL_RADIUS_LOGICAL_PX * scale).max(16.0),
            pod_w: px(POD_WIDTH_LOGICAL_PX),
            pod_h: px(POD_BASE_HEIGHT_LOGICAL_PX),
        }
    }
}

/// Monitor lookups the dock logic needs. Kept behind a trait so the decision code can be
/// unit-tested without a desktop.
trait MonitorProbe {
    fn work_rect_at(&self, x: i32, y: i32) -> Option<WorkRect>;
    fn exists_at(&self, x: i32, y: i32) -> bool;
}

struct NativeMonitors;

impl MonitorProbe for NativeMonitors {
    fn work_rect_at(&self, x: i32, y: i32) -> Option<WorkRect> {
        native::work_rect_at(x, y)
    }
    fn exists_at(&self, x: i32, y: i32) -> bool {
        native::monitor_exists_at(x, y)
    }
}

/// How long after a tap a second press still counts as a double press. Derived from the
/// system double-click time, which is measured press to press, so a bit more than half.
fn double_press_window() -> Duration {
    let system_ms = native::double_click_ms() as f64;
    Duration::from_millis((system_ms * 0.55).clamp(240.0, 330.0) as u64)
}

fn unix_millis_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

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
        if norm_logical >= 95.0 && best_impact.map_or(true, |b| norm_logical > b.normal_speed) {
            best_impact = Some(ImpactEvent {
                axis,
                normal_speed: norm_logical,
            });
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
        ORB_KICK_MIN_SPEED_REST_LOGICAL * scale
    } else {
        ORB_KICK_MIN_SPEED_AIR_LOGICAL * scale
    };
    if cspeed < min_kick_speed {
        return None;
    }

    let bcx = phys.px + orb_size * 0.5;
    let bcy = phys.py + orb_size * 0.5;
    let r = (ORB_VISUAL_RADIUS_LOGICAL_PX * scale).max(16.0);

    // Only a strike that comes from outside counts, so a cursor that is already resting on
    // the ball cannot kick it by wiggling.
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

/// A single tap pops the ball upward, leaning away from the side that was tapped.
fn apply_tap_bounce(phys: &mut OrbPhysics, cursor_x: f64, orb_size: f64, scale: f64) -> ImpactEvent {
    let scale = scale.max(0.5);
    let r = (ORB_VISUAL_RADIUS_LOGICAL_PX * scale).max(16.0);
    let ball_cx = phys.px + orb_size * 0.5;
    let lean = ((ball_cx - cursor_x) / r).clamp(-1.0, 1.0);
    phys.vx = (lean * 460.0 * scale).clamp(-2600.0 * scale, 2600.0 * scale);
    phys.vy = -940.0 * scale;
    phys.omega = (phys.vx / r) * 0.9;
    phys.sleeping = false;
    phys.combo = phys.combo.saturating_add(1);
    ImpactEvent {
        axis: "y",
        normal_speed: 920.0,
    }
}

/// Clamps one axis into [min, max] and reflects the velocity. Returns the impact speed.
fn bounce_axis(pos: &mut f64, vel: &mut f64, min: f64, max: f64, restitution: f64) -> Option<f64> {
    if *pos < min {
        *pos = min;
        let speed = vel.abs();
        if *vel < 0.0 {
            *vel = -*vel * restitution;
        }
        Some(speed)
    } else if *pos > max {
        *pos = max;
        let speed = vel.abs();
        if *vel > 0.0 {
            *vel = -*vel * restitution;
        }
        Some(speed)
    } else {
        None
    }
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
    mon: &dyn MonitorProbe,
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
    mon.exists_at(probe.0, probe.1)
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
    if let Some(rect) = native::work_rect_at(ref_x, ref_y) {
        return Some(rect);
    }
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

fn current_cursor(window: &WebviewWindow) -> Option<(f64, f64)> {
    if let Some((x, y)) = native::cursor_pos() {
        return Some((x as f64, y as f64));
    }
    window.cursor_position().ok().map(|c| (c.x, c.y))
}

fn current_window_rect(window: &WebviewWindow, hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    if let Some(rect) = native::window_rect(hwnd) {
        return Some(rect);
    }
    let pos = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    Some((pos.x, pos.y, size.width as i32, size.height as i32))
}

fn set_programmatic_pos(window: &WebviewWindow, dock: &Arc<Mutex<DockInner>>, x: i32, y: i32) {
    set_window_pos_fast(window, dock, x, y);
}

fn set_window_pos_fast(window: &WebviewWindow, dock: &Arc<Mutex<DockInner>>, x: i32, y: i32) {
    let hwnd = {
        let mut st = dock.lock().unwrap();
        st.last_programmatic_target = Some((x, y));
        st.ignore_moves_until = Instant::now() + Duration::from_millis(220);
        st.hwnd
    };
    if native::set_pos(hwnd, x, y) {
        return;
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
    let hwnd = {
        let mut st = dock.lock().unwrap();
        st.last_programmatic_target = Some((x, y));
        st.ignore_moves_until = Instant::now() + Duration::from_millis(280);
        st.hwnd
    };
    if native::set_rect(hwnd, x, y, w, h) {
        return;
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

/// Time-based glide with an ease-in-out curve, used when the ball flies back to its parking
/// spot. Sampling by elapsed time keeps the motion even when a frame runs late.
fn glide_window_timed(
    window: &WebviewWindow,
    dock: &Arc<Mutex<DockInner>>,
    from: (i32, i32),
    to: (i32, i32),
    duration_ms: u64,
) {
    if from == to {
        return;
    }
    {
        let mut st = dock.lock().unwrap();
        st.animating = true;
    }
    let total = Duration::from_millis(duration_ms.max(1)).as_secs_f64();
    let started = Instant::now();
    loop {
        let t = (started.elapsed().as_secs_f64() / total).min(1.0);
        let eased = if t < 0.5 {
            4.0 * t * t * t
        } else {
            1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
        };
        let x = (from.0 as f64 + (to.0 - from.0) as f64 * eased).round() as i32;
        let y = (from.1 as f64 + (to.1 - from.1) as f64 * eased).round() as i32;
        set_programmatic_pos(window, dock, x, y);
        if t >= 1.0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
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
        let seam = has_adjacent_monitor_beyond_edge(
            &NativeMonitors,
            x,
            y,
            pod_w,
            pod_h,
            area,
            DockEdge::Top,
        );
        let now = Instant::now();
        let (payload, saved) = {
            let mut st = dock.lock().unwrap();
            // Leave play mode and any gesture first: this restores the standby form and
            // work area that play mode borrowed, before the new placement overwrites them.
            release_play_state(&mut st);
            st.custom_dragging = false;
            st.pending_orb_return = None;
            st.scale = scale;
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
                let seam = has_adjacent_monitor_beyond_edge(
                    &NativeMonitors,
                    x,
                    y,
                    ww,
                    wh,
                    area,
                    saved.edge,
                );
                let now = Instant::now();
                {
                    let mut st = dock.lock().unwrap();
                    st.scale = scale;
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
        guard_clicks: bool,
    },
    EmitOnly(DockStatePayload),
    EmitOrbFx(OrbFxPayload),
}

/// Everything one watcher tick reads from the outside world.
struct TickEnv {
    now: Instant,
    cur: (f64, f64),
    win: (i32, i32, i32, i32),
    mouse_down: bool,
    escape_down: bool,
    double_press: Duration,
}

impl TickEnv {
    fn cursor_px(&self) -> (i32, i32) {
        (self.cur.0.round() as i32, self.cur.1.round() as i32)
    }
}

fn orb_home(st: &mut DockInner, m: &DockMetrics) -> (i32, i32) {
    match (st.orb_x, st.orb_y) {
        (Some(ox), Some(oy)) => (ox, oy),
        _ => {
            let pod_w = st.pod_w_phys.max(m.pod_w);
            let pod_h = st.pod_h_phys.max(m.pod_h);
            let p = compute_orb_pos_from_pod(
                st.dock_x,
                st.dock_y,
                pod_w,
                pod_h,
                m.orb_size,
                st.work_area,
                st.edge,
                m.margin,
            );
            st.orb_x = Some(p.0);
            st.orb_y = Some(p.1);
            p
        }
    }
}

/// Drops all play-mode and gesture bookkeeping without moving any window.
fn release_play_state(st: &mut DockInner) {
    if let Some(prev) = st.pre_play_standby.take() {
        st.standby_mode = prev;
    }
    if let Some(area) = st.pre_play_work_area.take() {
        st.work_area = area;
    }
    st.pre_play_orb_pos = None;
    st.play_mode = false;
    st.orb_coasting = false;
    st.orb_press = None;
    st.pending_tap = None;
    st.physics = OrbPhysics::default();
    st.last_emitted_angle = 0.0;
    st.vel_tracker.clear();
}

/// The single way out of play mode. Every exit (double press, close badge, right click,
/// Escape, idle timeout) goes through here, so none of them can leave a half-reset state.
fn request_exit_play(st: &mut DockInner, now: Instant, m: &DockMetrics) {
    if !st.play_mode {
        return;
    }
    let was_collapsed = st.orb_collapsed;
    let prev_standby = st.pre_play_standby.unwrap_or(st.standby_mode);
    let home = st.pre_play_orb_pos;
    release_play_state(st);
    st.custom_dragging = false;
    st.play_exited_at = Some(now);
    st.toggle_lock_until = now + Duration::from_millis(ORB_TOGGLE_LOCK_MS);

    if !was_collapsed {
        // Still morphing from the capsule into the ball: cancel that collapse.
        st.force_tuck = false;
        return;
    }
    if prev_standby == StandbyMode::Orb {
        let home = home.unwrap_or_else(|| orb_home(st, m));
        st.orb_x = Some(home.0);
        st.orb_y = Some(home.1);
        st.pending_orb_return = Some(home);
    } else {
        st.force_untuck = true;
    }
}

fn start_play_physics(st: &mut DockInner, now: Instant, start: (i32, i32), m: &DockMetrics) {
    st.play_mode = true;
    st.orb_coasting = false;
    st.orb_press = None;
    st.pending_tap = None;
    st.orb_hover_since = None;
    st.orb_hover_armed = false;
    st.physics = OrbPhysics {
        px: start.0 as f64,
        py: start.1 as f64,
        vx: 165.0 * m.scale,
        vy: -430.0 * m.scale,
        omega: 4.2,
        angle_deg: 0.0,
        sleeping: false,
        combo: 0,
    };
    st.last_emitted_angle = 0.0;
    st.last_physics_tick = now;
    st.last_play_interaction = now;
    st.last_kick_at = now;
    st.kick_block_until = now + Duration::from_millis(300);
    st.toggle_lock_until = now + Duration::from_millis(ORB_TOGGLE_LOCK_MS);
}

fn enter_play_from_orb(st: &mut DockInner, now: Instant, start: (i32, i32), m: &DockMetrics) {
    st.pre_play_orb_pos = Some((st.orb_x.unwrap_or(start.0), st.orb_y.unwrap_or(start.1)));
    st.pre_play_standby = None;
    st.pre_play_work_area = Some(st.work_area);
    start_play_physics(st, now, start, m);
}

fn enter_play_from_pod(
    st: &mut DockInner,
    now: Instant,
    cursor: Option<(i32, i32)>,
    m: &DockMetrics,
) {
    let home = orb_home(st, m);
    st.pre_play_orb_pos = Some(home);
    st.pre_play_standby = Some(st.standby_mode);
    st.pre_play_work_area = Some(st.work_area);
    st.force_tuck = true;
    let start = match cursor {
        Some((cx, cy)) => {
            let area = st.work_area;
            let min_x = area.x + m.margin;
            let max_x = (area.right() - m.orb_size - m.margin).max(min_x);
            let min_y = area.y + m.margin;
            let max_y = (area.bottom() - m.orb_size - m.margin).max(min_y);
            (
                (cx - m.orb_size / 2).clamp(min_x, max_x),
                (cy - m.orb_size / 2).clamp(min_y, max_y),
            )
        }
        None => home,
    };
    start_play_physics(st, now, start, m);
}

#[derive(Debug, PartialEq, Eq)]
enum PressOutcome {
    Ignored,
    Pressed,
    EnteredPlay,
    ExitedPlay,
}

/// Handles the primary button going down on the ball. A second press that arrives while a
/// tap is still waiting toggles play mode at once; anything else starts a press that the
/// watcher follows until the button comes back up.
fn orb_press_begin(
    st: &mut DockInner,
    now: Instant,
    cursor: (i32, i32),
    win_pos: (i32, i32),
    grab_logical: (f64, f64),
    m: &DockMetrics,
) -> PressOutcome {
    if !st.orb_collapsed
        || st.animating
        || st.pending_orb_return.is_some()
        || st.press_consumed
        || now < st.toggle_lock_until
    {
        return PressOutcome::Ignored;
    }
    if let Some(prev) = st.orb_press.take() {
        // A new press can only follow a release, so the watcher simply has not seen that
        // release yet. Count the earlier press as the tap it was.
        if !prev.moved
            && now.duration_since(prev.started) <= Duration::from_millis(ORB_TAP_MAX_MS + 330)
        {
            st.pending_tap = Some(now);
        }
    }
    let grab = (
        ((grab_logical.0 * m.scale).round() as i32).clamp(0, m.orb_size),
        ((grab_logical.1 * m.scale).round() as i32).clamp(0, m.orb_size),
    );
    // The window should still be under the grab point. If it is far off, this command is
    // stale (the press it describes is over) and must not teleport the ball.
    let expected = (cursor.0 - grab.0, cursor.1 - grab.1);
    let drift = ((expected.0 - win_pos.0) as f64).hypot((expected.1 - win_pos.1) as f64);
    if drift > 160.0 * m.scale {
        return PressOutcome::Ignored;
    }

    st.orb_hover_since = None;
    st.orb_expand_suppress_until = now + Duration::from_millis(620);
    st.kick_block_until = now + Duration::from_millis(ORB_KICK_BLOCK_AFTER_PRESS_MS);
    st.last_play_interaction = now;
    st.last_hover_at = now;

    if st.pending_tap.take().is_some() {
        st.press_consumed = true;
        if st.play_mode {
            request_exit_play(st, now, m);
            return PressOutcome::ExitedPlay;
        }
        enter_play_from_orb(st, now, win_pos, m);
        return PressOutcome::EnteredPlay;
    }

    st.orb_coasting = false;
    st.vel_tracker.clear();
    if st.play_mode {
        // Caught: the ball stops where it is and stays under the cursor.
        st.physics.vx = 0.0;
        st.physics.vy = 0.0;
        st.physics.omega = 0.0;
    }
    st.orb_press = Some(OrbPress {
        started: now,
        press_cursor: cursor,
        grab_offset: grab,
        moved: false,
    });
    PressOutcome::Pressed
}

/// Collapse request from Escape or the hide button. Returns true when the window should
/// be hidden to the tray instead.
fn request_tuck(st: &mut DockInner, now: Instant, m: &DockMetrics) -> bool {
    if st.play_mode {
        request_exit_play(st, now, m);
        return false;
    }
    // The same Escape press that just ended play mode must not also collapse the capsule.
    if st
        .play_exited_at
        .map_or(false, |t| now.duration_since(t) < Duration::from_millis(450))
    {
        return false;
    }
    if st.tucked || st.orb_collapsed {
        return false;
    }
    if st.standby_mode == StandbyMode::Orb || st.edge != DockEdge::None {
        st.force_tuck = true;
        return false;
    }
    true
}

fn park_orb_at(
    st: &mut DockInner,
    m: &DockMetrics,
    mon: &dyn MonitorProbe,
    area: WorkRect,
    pos: (i32, i32),
) -> WatcherAction {
    let min_ox = area.x + m.margin;
    let max_ox = (area.right() - m.orb_size - m.margin).max(min_ox);
    let min_oy = area.y + m.margin;
    let max_oy = (area.bottom() - m.orb_size - m.margin).max(min_oy);
    let ox = pos.0.clamp(min_ox, max_ox);
    let oy = pos.1.clamp(min_oy, max_oy);
    let pod_w = st.pod_w_phys.max(m.pod_w);
    let pod_h = st.pod_h_phys.max(m.pod_h);
    let (px, py, new_edge) =
        compute_pod_pos_from_orb(ox, oy, m.orb_size, pod_w, pod_h, area, m.margin);
    st.orb_x = Some(ox);
    st.orb_y = Some(oy);
    st.dock_x = px;
    st.dock_y = py;
    st.edge = new_edge;
    st.work_area = area;
    st.seam_mode = has_adjacent_monitor_beyond_edge(mon, px, py, pod_w, pod_h, area, new_edge);
    WatcherAction::SaveOrbDrag {
        to: (ox, oy),
        saved: st.to_saved(),
        payload: st.payload(),
    }
}

fn snap_pod_at(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    mon: &dyn MonitorProbe,
    area: WorkRect,
    pos: (i32, i32),
) -> WatcherAction {
    let now = env.now;
    let (_, _, ww, wh) = env.win;
    st.preview = DockEdge::None;
    // Anchor on the base capsule height so dragging with a drawer open still remembers the true home y.
    let anchor_h = st.pod_h_phys.max(m.pod_h);
    let (sx, sy, new_edge) =
        snap_window_rect(pos.0, pos.1, ww, anchor_h, area, m.threshold, m.margin);
    let seam = has_adjacent_monitor_beyond_edge(mon, sx, sy, ww, anchor_h, area, new_edge);
    st.edge = new_edge;
    st.dock_x = sx;
    st.dock_y = sy;
    st.work_area = area;
    st.seam_mode = seam;
    // Re-derive the companion ball anchor whenever the user repositions the capsule.
    let (ox, oy) = compute_orb_pos_from_pod(
        sx, sy, ww, anchor_h, m.orb_size, area, new_edge, m.margin,
    );
    st.orb_x = Some(ox);
    st.orb_y = Some(oy);
    st.tucked = false;
    st.orb_collapsed = false;
    st.idle = false;
    st.last_hover_at = now;
    st.grace_until = now + Duration::from_millis(1400);
    let vis_to = compute_visible_pod_pos(sx, sy, ww, wh, area, new_edge, m.margin);
    WatcherAction::SnapPod {
        to: vis_to,
        saved: st.to_saved(),
        payload: st.payload(),
    }
}

fn expand_orb_to_pod(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    guard_clicks: bool,
) -> WatcherAction {
    let now = env.now;
    st.orb_hover_since = None;
    st.orb_collapsed = false;
    st.idle = false;
    st.last_hover_at = now;
    st.grace_until = now + Duration::from_millis(1200);
    let pod_w = st.pod_w_phys.max(m.pod_w);
    let pod_h = st.pod_h_phys.max(m.pod_h);
    let to = compute_visible_pod_pos(
        st.dock_x,
        st.dock_y,
        pod_w,
        pod_h,
        st.work_area,
        st.edge,
        m.margin,
    );
    WatcherAction::ExitOrb {
        pod_pos: to,
        pod_w,
        pod_h,
        payload: st.payload(),
        guard_clicks,
    }
}

/// One step of the dock state machine. Reads nothing but its arguments, so it can run
/// under the dock mutex without touching the UI thread, and can be unit-tested.
fn decide_tick(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    mon: &dyn MonitorProbe,
) -> WatcherAction {
    let now = env.now;
    let (wx, wy, ww, wh) = env.win;

    let prev_cursor = st.last_cursor;
    st.last_cursor = Some((now, env.cur.0, env.cur.1));
    let escape_edge = env.escape_down && !st.escape_was_down;
    st.escape_was_down = env.escape_down;
    if st.press_consumed && !env.mouse_down {
        st.press_consumed = false;
    }

    if !st.orb_collapsed && !st.tucked && ww >= (m.pod_w - 20) {
        st.pod_w_phys = ww;
        if wh <= m.pod_h + (24.0 * m.scale).round() as i32 {
            st.pod_h_phys = wh;
        }
    }

    // 0. The ball flies back to its parking spot after play mode ends.
    if let Some(return_to) = st.pending_orb_return.take() {
        st.orb_press = None;
        st.pending_tap = None;
        st.play_mode = false;
        st.orb_coasting = false;
        st.custom_dragging = false;
        st.pending_user_move = false;
        st.physics = OrbPhysics::default();
        st.last_emitted_angle = 0.0;
        st.orb_hover_since = None;
        st.orb_hover_armed = false;
        st.orb_expand_suppress_until = now + Duration::from_millis(520);
        if !st.orb_collapsed {
            return WatcherAction::EmitOnly(st.payload());
        }
        return WatcherAction::GlideOrbReturn {
            from: (wx, wy),
            to: return_to,
            payload: st.payload(),
        };
    }

    // 1. The capsule is being dragged.
    if st.custom_dragging {
        if st.orb_collapsed || st.tucked {
            st.custom_dragging = false;
        } else {
            return decide_pod_drag(st, env, m, mon);
        }
    }

    // 2. Something asked for the capsule to be shown.
    if st.force_untuck {
        st.force_untuck = false;
        if st.tucked || st.orb_collapsed {
            st.force_tuck = false;
            release_play_state(st);
            st.idle = false;
            st.last_hover_at = now;
            st.grace_until = now + Duration::from_millis(2400);
            if st.orb_collapsed {
                st.orb_collapsed = false;
                st.orb_hover_since = None;
                let pod_w = st.pod_w_phys.max(m.pod_w);
                let pod_h = st.pod_h_phys.max(m.pod_h);
                let to = compute_visible_pod_pos(
                    st.dock_x,
                    st.dock_y,
                    pod_w,
                    pod_h,
                    st.work_area,
                    st.edge,
                    m.margin,
                );
                return WatcherAction::ExitOrb {
                    pod_pos: to,
                    pod_w,
                    pod_h,
                    payload: st.payload(),
                    guard_clicks: false,
                };
            }
            st.tucked = false;
            return WatcherAction::UntuckEdge {
                from: (wx, wy),
                to: (st.dock_x, st.dock_y),
                payload: st.payload(),
            };
        }
    }

    // 3. Something asked for the capsule to be put away.
    if st.force_tuck {
        st.force_tuck = false;
        if !st.tucked && !st.orb_collapsed {
            st.pinned = false;
            st.ui_hold = false;
            if st.standby_mode == StandbyMode::Orb || st.play_mode {
                let orb_pos = if st.play_mode {
                    (st.physics.px.round() as i32, st.physics.py.round() as i32)
                } else {
                    orb_home(st, m)
                };
                st.last_physics_tick = now;
                return WatcherAction::EnterOrb {
                    orb_pos,
                    orb_size: m.orb_size,
                    play: st.play_mode,
                };
            } else if st.edge != DockEdge::None {
                return WatcherAction::TuckEdge {
                    edge: st.edge,
                    dock_x: st.dock_x,
                    dock_y: st.dock_y,
                    area: st.work_area,
                    seam: st.seam_mode,
                    peek: m.peek,
                };
            }
        }
    }

    // 4. The window was moved by the system rather than by us.
    if st.pending_user_move {
        if st.orb_press.is_some() || st.pending_tap.is_some() || st.play_mode || st.orb_coasting {
            st.pending_user_move = false;
        } else {
            return decide_os_move(st, env, m, mon);
        }
    }

    if st.orb_collapsed {
        return decide_orb(st, env, m, mon, prev_cursor, escape_edge);
    }
    decide_pod_visible(st, env, m)
}

fn decide_pod_drag(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    mon: &dyn MonitorProbe,
) -> WatcherAction {
    let now = env.now;
    let (wx, wy, ww, wh) = env.win;
    let (cx, cy) = env.cursor_px();
    let target = (cx - st.drag_offset.0, cy - st.drag_offset.1);
    let live_area = mon
        .work_rect_at(target.0 + ww / 2, target.1 + wh.min(160) / 2)
        .unwrap_or(st.work_area);
    st.last_hover_at = now;

    if env.mouse_down {
        let t = now.duration_since(st.epoch).as_secs_f64();
        st.vel_tracker.push(t, target.0 as f64, target.1 as f64);
        let prev = detect_snap_edge(target.0, target.1, ww, wh, live_area, m.threshold);
        let payload = if prev != st.preview {
            st.preview = prev;
            Some(st.payload())
        } else {
            None
        };
        if target == (wx, wy) && payload.is_none() {
            return WatcherAction::None;
        }
        return WatcherAction::MoveFast {
            to: target,
            payload,
            fx: None,
        };
    }

    st.custom_dragging = false;
    st.pending_user_move = false;
    let (vx, vy) = st.vel_tracker.velocity(4300.0 * m.scale);
    st.vel_tracker.clear();
    let projected = (
        wx + (vx * 0.055).round() as i32,
        wy + (vy * 0.055).round() as i32,
    );
    snap_pod_at(st, env, m, mon, live_area, projected)
}

fn decide_os_move(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    mon: &dyn MonitorProbe,
) -> WatcherAction {
    let (wx, wy, ww, wh) = env.win;
    let live_area = mon
        .work_rect_at(wx + ww / 2, wy + wh.min(160) / 2)
        .unwrap_or(st.work_area);
    if st.orb_collapsed {
        if env.mouse_down {
            return WatcherAction::None;
        }
        st.pending_user_move = false;
        return park_orb_at(st, m, mon, live_area, (wx, wy));
    }
    if env.mouse_down {
        let prev = detect_snap_edge(wx, wy, ww, wh, live_area, m.threshold);
        if prev != st.preview {
            st.preview = prev;
            return WatcherAction::EmitOnly(st.payload());
        }
        return WatcherAction::None;
    }
    st.pending_user_move = false;
    snap_pod_at(st, env, m, mon, live_area, (wx, wy))
}

fn decide_orb(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    mon: &dyn MonitorProbe,
    prev_cursor: Option<(Instant, f64, f64)>,
    escape_edge: bool,
) -> WatcherAction {
    let now = env.now;
    let (wx, wy, _, _) = env.win;
    let (cx, cy) = env.cursor_px();
    let half = m.orb_size as f64 * 0.5;

    // Escape leaves play mode even when the ball window has no keyboard focus, as long as
    // the cursor is near the ball. Polled, so it works in every sub-state below.
    if st.play_mode && escape_edge {
        let dist = (env.cur.0 - (wx as f64 + half)).hypot(env.cur.1 - (wy as f64 + half));
        if dist <= m.orb_radius + 56.0 * m.scale {
            request_exit_play(st, now, m);
            return WatcherAction::None;
        }
    }

    // A. A press that began on the ball.
    if let Some(mut press) = st.orb_press {
        st.orb_hover_since = None;
        st.last_hover_at = now;

        if env.mouse_down {
            if !press.moved {
                let travel = ((cx - press.press_cursor.0) as f64)
                    .hypot((cy - press.press_cursor.1) as f64);
                if travel > ORB_DRAG_SLOP_LOGICAL_PX * m.scale {
                    press.moved = true;
                    st.orb_press = Some(press);
                }
            }
            // Outside play mode the ball stays put until the press turns into a drag.
            // In play mode it is held under the cursor from the first moment.
            if !(st.play_mode || press.moved) {
                return WatcherAction::None;
            }
            let raw = (cx - press.grab_offset.0, cy - press.grab_offset.1);
            let area = mon
                .work_rect_at(raw.0 + m.orb_size / 2, raw.1 + m.orb_size / 2)
                .unwrap_or(st.work_area);
            st.work_area = area;
            let bounds = OrbPhysicsBounds::from_work_area(area, m.orb_size, m.orb_inset);
            let nx = (raw.0 as f64).clamp(bounds.min_x, bounds.max_x).round() as i32;
            let ny = (raw.1 as f64).clamp(bounds.min_y, bounds.max_y).round() as i32;
            let t = now.duration_since(st.epoch).as_secs_f64();
            st.vel_tracker.push(t, nx as f64, ny as f64);
            st.physics.px = nx as f64;
            st.physics.py = ny as f64;
            st.physics.vx = 0.0;
            st.physics.vy = 0.0;
            st.physics.omega = 0.0;
            st.last_physics_tick = now;
            if (nx, ny) == (wx, wy) {
                return WatcherAction::None;
            }
            return WatcherAction::MoveFast {
                to: (nx, ny),
                payload: None,
                fx: None,
            };
        }

        // The button came up.
        st.orb_press = None;
        st.last_play_interaction = now;
        st.kick_block_until = now + Duration::from_millis(ORB_KICK_BLOCK_AFTER_PRESS_MS);
        if press.moved {
            return release_orb_drag(st, env, m, mon);
        }
        if now.duration_since(press.started) <= Duration::from_millis(ORB_TAP_MAX_MS) {
            // A tap. Wait briefly for a second press before acting on it; in play mode the
            // ball holds still meanwhile so that second press can land.
            st.pending_tap = Some(now + env.double_press);
            if st.play_mode {
                return WatcherAction::EmitOrbFx(OrbFxPayload::charge(
                    st.physics.angle_deg,
                    st.physics.combo,
                ));
            }
            return WatcherAction::None;
        }
        // Held still and let go: in play mode the ball simply drops.
        if st.play_mode {
            st.physics.sleeping = false;
            st.last_physics_tick = now;
        }
        return WatcherAction::None;
    }

    // B. A single tap waiting to see whether a second press follows.
    if let Some(deadline) = st.pending_tap {
        st.orb_hover_since = None;
        st.last_hover_at = now;
        if now < deadline {
            return WatcherAction::None;
        }
        st.pending_tap = None;
        if st.play_mode {
            let hit = apply_tap_bounce(&mut st.physics, env.cur.0, m.orb_size as f64, m.scale);
            st.last_physics_tick = now;
            st.last_emitted_angle = st.physics.angle_deg;
            return WatcherAction::EmitOrbFx(OrbFxPayload::impact(
                st.physics.angle_deg,
                st.physics.combo,
                Some(hit),
            ));
        }
        return expand_orb_to_pod(st, env, m, true);
    }

    if st.play_mode {
        return decide_play(st, env, m, prev_cursor);
    }
    if st.orb_coasting {
        return decide_coast(st, env, m, mon);
    }
    decide_hover(st, env, m, prev_cursor)
}

fn release_orb_drag(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    mon: &dyn MonitorProbe,
) -> WatcherAction {
    let now = env.now;
    let (wx, wy, _, _) = env.win;
    let area = st.work_area;
    let (vx, vy) = st.vel_tracker.velocity(4300.0 * m.scale);
    st.vel_tracker.clear();
    let bounds = OrbPhysicsBounds::from_work_area(area, m.orb_size, m.orb_inset);
    let ox = (wx as f64).clamp(bounds.min_x, bounds.max_x);
    let oy = (wy as f64).clamp(bounds.min_y, bounds.max_y);

    if st.play_mode {
        // Thrown: the release velocity carries straight into the physics.
        st.physics.px = ox;
        st.physics.py = oy;
        st.physics.vx = vx;
        st.physics.vy = vy;
        st.physics.omega = (vx / m.orb_radius) * 0.65;
        st.physics.sleeping = false;
        st.last_physics_tick = now;
        return WatcherAction::None;
    }

    st.orb_hover_armed = false;
    if vx.hypot(vy) >= ORB_COAST_MIN_SPEED_LOGICAL * m.scale {
        st.orb_coasting = true;
        st.physics.px = ox;
        st.physics.py = oy;
        st.physics.vx = vx * 0.62;
        st.physics.vy = vy * 0.62;
        st.physics.sleeping = false;
        st.last_physics_tick = now;
        return WatcherAction::None;
    }
    st.orb_expand_suppress_until = now + Duration::from_millis(450);
    park_orb_at(st, m, mon, area, (ox.round() as i32, oy.round() as i32))
}

fn cursor_speed(env: &TickEnv, prev_cursor: Option<(Instant, f64, f64)>) -> f64 {
    prev_cursor.map_or(0.0, |(t, x, y)| {
        let dt = env.now.duration_since(t).as_secs_f64();
        if dt > 0.0005 {
            (env.cur.0 - x).hypot(env.cur.1 - y) / dt
        } else {
            0.0
        }
    })
}

fn decide_play(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    prev_cursor: Option<(Instant, f64, f64)>,
) -> WatcherAction {
    let now = env.now;
    let (wx, wy, _, _) = env.win;
    let bounds = OrbPhysicsBounds::from_work_area(st.work_area, m.orb_size, m.orb_inset);
    let dt = now
        .duration_since(st.last_physics_tick)
        .as_secs_f64()
        .clamp(0.001, 0.032);
    st.last_physics_tick = now;

    // Nobody has played for a while and the ball is at rest: go back on duty.
    if st.physics.sleeping
        && now.duration_since(st.last_play_interaction)
            >= Duration::from_secs(ORB_PLAY_IDLE_RETURN_SECS)
        && now.duration_since(st.last_kick_at) >= Duration::from_secs(ORB_PLAY_IDLE_AFTER_KICK_SECS)
    {
        request_exit_play(st, now, m);
        return WatcherAction::None;
    }

    let half = m.orb_size as f64 * 0.5;
    let ball = (st.physics.px + half, st.physics.py + half);
    let cursor_dist = (env.cur.0 - ball.0).hypot(env.cur.1 - ball.1);
    let speed = cursor_speed(env, prev_cursor);

    let mut impact: Option<ImpactEvent> = None;
    let kicks_allowed = !env.mouse_down
        && now >= st.kick_block_until
        && now.duration_since(st.last_kick_at) >= Duration::from_millis(105);
    if kicks_allowed {
        if let Some((prev_t, prev_x, prev_y)) = prev_cursor {
            let c_dt = now.duration_since(prev_t).as_secs_f64();
            if let Some(hit) = apply_cursor_kick(
                &mut st.physics,
                (prev_x, prev_y),
                env.cur,
                c_dt,
                m.orb_size as f64,
                bounds,
                m.scale,
            ) {
                st.last_kick_at = now;
                impact = Some(hit);
            }
        }
    }

    // A slow cursor resting on a ball that is only rolling settles it, so it can be
    // pressed or closed without chasing it.
    if !st.physics.sleeping
        && st.physics.py >= bounds.max_y - 1.0
        && st.physics.vy.abs() < 60.0 * m.scale
        && st.physics.vx.abs() < 320.0 * m.scale
        && cursor_dist <= m.orb_radius + 6.0 * m.scale
        && speed < 250.0 * m.scale
    {
        st.physics.vx = 0.0;
    }

    if let Some(hit) = step_orb_gravity(&mut st.physics, dt, bounds, m.scale) {
        if impact.map_or(true, |k| hit.normal_speed > k.normal_speed) {
            impact = Some(hit);
        }
    }

    let nx = st.physics.px.round() as i32;
    let ny = st.physics.py.round() as i32;
    let angle_delta = (st.physics.angle_deg - st.last_emitted_angle).abs();
    let fx = if impact.is_some() || angle_delta >= 2.2 {
        st.last_emitted_angle = st.physics.angle_deg;
        Some(OrbFxPayload::impact(
            st.physics.angle_deg,
            st.physics.combo,
            impact,
        ))
    } else {
        None
    };

    if (nx, ny) != (wx, wy) {
        WatcherAction::MoveFast {
            to: (nx, ny),
            payload: None,
            fx,
        }
    } else if let Some(fx_only) = fx {
        WatcherAction::EmitOrbFx(fx_only)
    } else {
        WatcherAction::None
    }
}

fn decide_coast(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    mon: &dyn MonitorProbe,
) -> WatcherAction {
    let now = env.now;
    let dt = now
        .duration_since(st.last_physics_tick)
        .as_secs_f64()
        .clamp(0.001, 0.032);
    st.last_physics_tick = now;
    let area = st.work_area;
    let min_ox = (area.x + m.margin) as f64;
    let max_ox = ((area.right() - m.orb_size - m.margin) as f64).max(min_ox);
    let min_oy = (area.y + m.margin) as f64;
    let max_oy = ((area.bottom() - m.orb_size - m.margin) as f64).max(min_oy);

    let decay = (-7.5 * dt).exp();
    st.physics.vx *= decay;
    st.physics.vy *= decay;
    st.physics.px += st.physics.vx * dt;
    st.physics.py += st.physics.vy * dt;

    let mut impact: Option<ImpactEvent> = None;
    if let Some(speed) = bounce_axis(&mut st.physics.px, &mut st.physics.vx, min_ox, max_ox, 0.62) {
        if speed / m.scale > 120.0 {
            impact = Some(ImpactEvent {
                axis: "x",
                normal_speed: speed / m.scale,
            });
        }
    }
    if let Some(speed) = bounce_axis(&mut st.physics.py, &mut st.physics.vy, min_oy, max_oy, 0.62) {
        if speed / m.scale > 120.0 {
            impact = Some(ImpactEvent {
                axis: "y",
                normal_speed: speed / m.scale,
            });
        }
    }

    let nx = st.physics.px.round() as i32;
    let ny = st.physics.py.round() as i32;
    if st.physics.vx.hypot(st.physics.vy) < 28.0 * m.scale {
        st.orb_coasting = false;
        st.physics = OrbPhysics::default();
        st.orb_expand_suppress_until = now + Duration::from_millis(380);
        return park_orb_at(st, m, mon, area, (nx, ny));
    }
    WatcherAction::MoveFast {
        to: (nx, ny),
        payload: None,
        fx: impact.map(|hit| OrbFxPayload::impact(0.0, 0, Some(hit))),
    }
}

/// Hover intent: the ball opens into the capsule only after the cursor has stayed on it
/// for the dwell time without a button held and without sweeping through.
fn decide_hover(
    st: &mut DockInner,
    env: &TickEnv,
    m: &DockMetrics,
    prev_cursor: Option<(Instant, f64, f64)>,
) -> WatcherAction {
    let now = env.now;
    let (wx, wy, _, _) = env.win;
    let half = m.orb_size as f64 * 0.5;
    let hover_r = (ORB_VISUAL_RADIUS_LOGICAL_PX + 2.0) * m.scale;
    let dist = (env.cur.0 - (wx as f64 + half)).hypot(env.cur.1 - (wy as f64 + half));

    if dist > hover_r {
        st.orb_hover_since = None;
        if dist > hover_r + 10.0 * m.scale {
            st.orb_hover_armed = true;
        }
        return WatcherAction::None;
    }
    if env.mouse_down {
        // Never change shape under a held button: it is either a press on the ball or
        // something being dragged across it.
        st.orb_hover_since = None;
        let hold_off = now + Duration::from_millis(300);
        if hold_off > st.orb_expand_suppress_until {
            st.orb_expand_suppress_until = hold_off;
        }
        return WatcherAction::None;
    }
    if !st.orb_hover_armed || now < st.orb_expand_suppress_until {
        st.orb_hover_since = None;
        return WatcherAction::None;
    }
    if cursor_speed(env, prev_cursor) > ORB_HOVER_MAX_SPEED_LOGICAL * m.scale {
        st.orb_hover_since = Some(now);
        return WatcherAction::None;
    }
    match st.orb_hover_since {
        None => {
            st.orb_hover_since = Some(now);
            WatcherAction::None
        }
        Some(since) if now.duration_since(since) >= Duration::from_millis(ORB_HOVER_EXPAND_MS) => {
            expand_orb_to_pod(st, env, m, true)
        }
        Some(_) => WatcherAction::None,
    }
}

fn decide_pod_visible(st: &mut DockInner, env: &TickEnv, m: &DockMetrics) -> WatcherAction {
    let now = env.now;
    let (wx, wy, ww, wh) = env.win;
    let (cx, cy) = env.cursor_px();

    // The capsule stays anchored to (dock_x, dock_y). It shifts up only while an open
    // drawer would run past the bottom of the work area, and returns once the drawer closes.
    let (vis_x, vis_y) = compute_visible_pod_pos(
        st.dock_x,
        st.dock_y,
        ww,
        wh,
        st.work_area,
        st.edge,
        m.margin,
    );
    if !st.tucked
        && ((vis_x - wx).abs() > 2 || (vis_y - wy).abs() > 2)
        && now >= st.ignore_moves_until
    {
        return WatcherAction::AdjustOnScreen {
            to: (vis_x, vis_y),
        };
    }

    if st.standby_mode == StandbyMode::Orb {
        let pad = (8.0 * m.scale).round() as i32;
        let inside = is_cursor_in_pod(cx, cy, wx, wy, ww, wh, pad);
        if inside || st.pinned || st.ui_hold || env.mouse_down {
            st.last_hover_at = now;
            if st.idle {
                st.idle = false;
                return WatcherAction::EmitOnly(st.payload());
            }
            return WatcherAction::None;
        }
        if now >= st.grace_until
            && now.duration_since(st.last_hover_at) >= Duration::from_millis(1350)
        {
            let orb_pos = orb_home(st, m);
            return WatcherAction::EnterOrb {
                orb_pos,
                orb_size: m.orb_size,
                play: false,
            };
        }
        return WatcherAction::None;
    }

    match st.edge {
        DockEdge::Top | DockEdge::Left | DockEdge::Right => {
            if st.tucked {
                if !env.mouse_down
                    && is_cursor_in_tucked_handle(
                        cx,
                        cy,
                        st.dock_x,
                        st.dock_y,
                        ww,
                        wh,
                        st.work_area,
                        m.scale,
                        st.edge,
                    )
                {
                    st.tucked = false;
                    st.idle = false;
                    st.last_hover_at = now;
                    st.grace_until = now + Duration::from_millis(900);
                    return WatcherAction::UntuckEdge {
                        from: (wx, wy),
                        to: (st.dock_x, st.dock_y),
                        payload: st.payload(),
                    };
                }
                return WatcherAction::None;
            }
            let pad = (8.0 * m.scale).round() as i32;
            let inside = is_cursor_in_pod(cx, cy, wx, wy, ww, wh, pad);
            if inside || st.pinned || st.ui_hold || env.mouse_down {
                st.last_hover_at = now;
                return WatcherAction::None;
            }
            if now >= st.grace_until
                && now.duration_since(st.last_hover_at) >= Duration::from_millis(1200)
            {
                return WatcherAction::TuckEdge {
                    edge: st.edge,
                    dock_x: st.dock_x,
                    dock_y: st.dock_y,
                    area: st.work_area,
                    seam: st.seam_mode,
                    peek: m.peek,
                };
            }
            WatcherAction::None
        }
        DockEdge::None => {
            let pad = (6.0 * m.scale).round() as i32;
            let inside = is_cursor_in_pod(cx, cy, wx, wy, ww, wh, pad);
            if inside || st.pinned || st.ui_hold || env.mouse_down {
                st.last_hover_at = now;
                if st.idle {
                    st.idle = false;
                    return WatcherAction::EmitOnly(st.payload());
                }
                return WatcherAction::None;
            }
            if !st.idle
                && now >= st.grace_until
                && now.duration_since(st.last_hover_at) >= Duration::from_millis(2200)
            {
                st.idle = true;
                return WatcherAction::EmitOnly(st.payload());
            }
            WatcherAction::None
        }
    }
}

fn tick_interval_ms(st: &DockInner) -> u64 {
    if st.custom_dragging
        || st.orb_press.is_some()
        || st.pending_tap.is_some()
        || st.pending_orb_return.is_some()
        || st.orb_coasting
        || st.force_tuck
        || st.force_untuck
        || (st.play_mode && st.orb_collapsed && !st.physics.sleeping)
    {
        8
    } else if st.play_mode && st.orb_collapsed {
        16
    } else if st.orb_collapsed {
        24
    } else {
        30
    }
}

fn apply_watcher_action(
    window: &WebviewWindow,
    dock: &Arc<Mutex<DockInner>>,
    m: &DockMetrics,
    action: WatcherAction,
) {
    match action {
        WatcherAction::None => {}
        WatcherAction::MoveFast { to, payload, fx } => {
            set_window_pos_fast(window, dock, to.0, to.1);
            if let Some(p) = payload {
                let _ = window.emit("dock-state", p);
            }
            if let Some(f) = fx {
                let _ = window.emit("orb-fx", f);
            }
        }
        WatcherAction::GlideOrbReturn { from, to, payload } => {
            let _ = window.emit("dock-state", payload);
            let _ = window.emit("orb-fx", OrbFxPayload::reset());
            let distance = ((to.0 - from.0) as f64).hypot((to.1 - from.1) as f64) / m.scale;
            let duration_ms = (160.0 + 0.25 * distance).clamp(180.0, 420.0) as u64;
            glide_window_timed(window, dock, from, to, duration_ms);
        }
        WatcherAction::EmitOnly(payload) => {
            let _ = window.emit("dock-state", payload);
        }
        WatcherAction::EmitOrbFx(fx) => {
            let _ = window.emit("orb-fx", fx);
        }
        WatcherAction::AdjustOnScreen { to } => {
            set_programmatic_pos(window, dock, to.0, to.1);
        }
        WatcherAction::SnapPod { to, saved, payload } => {
            let _ = window.set_ignore_cursor_events(false);
            let hwnd = dock.lock().unwrap().hwnd;
            if let Some((cur_x, cur_y, _, _)) = current_window_rect(window, hwnd) {
                if (cur_x, cur_y) != to {
                    glide_window(window, dock, (cur_x, cur_y), to, 6, 14);
                }
            }
            persist_saved_dock(&saved);
            let _ = window.emit("dock-state", payload);
        }
        WatcherAction::SaveOrbDrag { to, saved, payload } => {
            let hwnd = dock.lock().unwrap().hwnd;
            if let Some((cur_x, cur_y, _, _)) = current_window_rect(window, hwnd) {
                if (cur_x, cur_y) != to {
                    set_programmatic_pos(window, dock, to.0, to.1);
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
            set_window_rect_atomic(window, dock, dock_x, dock_y, m.pod_w, m.pod_h);
            let target = tucked_window_pos(dock_x, dock_y, m.pod_w, m.pod_h, area, peek, edge, seam);
            glide_window(window, dock, (dock_x, dock_y), target, 10, 14);
            let payload = {
                let mut st = dock.lock().unwrap();
                st.pod_h_phys = m.pod_h;
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
            glide_window(window, dock, from, to, 10, 14);
        }
        WatcherAction::EnterOrb {
            orb_pos,
            orb_size,
            play,
        } => {
            let payload = {
                let mut st = dock.lock().unwrap();
                if play && !st.play_mode {
                    // Play mode was cancelled while the capsule was still collapsing.
                    None
                } else {
                    let now = Instant::now();
                    st.pod_h_phys = m.pod_h;
                    st.orb_collapsed = true;
                    st.tucked = false;
                    st.idle = true;
                    st.orb_hover_since = None;
                    st.orb_hover_armed = false;
                    st.orb_expand_suppress_until = now + Duration::from_millis(480);
                    st.ignore_moves_until = now + Duration::from_millis(320);
                    Some(st.payload())
                }
            };
            let Some(payload) = payload else {
                return;
            };
            let _ = window.emit(
                "collapse-drawers",
                serde_json::json!({ "immediate": true }),
            );
            let _ = window.set_ignore_cursor_events(false);
            let _ = window.emit("dock-state", payload);
            if !play {
                let _ = window.emit("orb-fx", OrbFxPayload::reset());
            }
            std::thread::sleep(Duration::from_millis(16));
            set_window_rect_atomic(window, dock, orb_pos.0, orb_pos.1, orb_size, orb_size);
        }
        WatcherAction::ExitOrb {
            pod_pos,
            pod_w,
            pod_h,
            payload,
            guard_clicks,
        } => {
            let _ = window.set_ignore_cursor_events(false);
            let _ = window.emit("orb-fx", OrbFxPayload::reset());
            set_window_rect_atomic(window, dock, pod_pos.0, pod_pos.1, pod_w, pod_h);
            let _ = window.emit("dock-state", payload);
            if guard_clicks {
                // The capsule just appeared under a cursor that was aiming at the ball.
                let _ = window.emit("pod-click-guard", ());
            }
        }
    }
}

fn start_dock_watcher(
    window: WebviewWindow,
    dock: Arc<Mutex<DockInner>>,
    waker: Arc<OnceLock<std::thread::Thread>>,
    quitting: Arc<AtomicBool>,
) {
    #[cfg(target_os = "windows")]
    let hwnd: isize = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    #[cfg(not(target_os = "windows"))]
    let hwnd: isize = 0;
    {
        let mut st = dock.lock().unwrap();
        st.hwnd = hwnd;
        st.scale = window.scale_factor().unwrap_or(1.0);
    }

    let handle = std::thread::spawn(move || {
        let mon = NativeMonitors;
        let double_press = double_press_window();
        while !quitting.load(Ordering::SeqCst) {
            let (interval_ms, scale) = {
                let st = dock.lock().unwrap();
                (tick_interval_ms(&st), st.scale)
            };
            // Parked rather than asleep, so a command can wake this thread at once.
            std::thread::park_timeout(Duration::from_millis(interval_ms));
            if quitting.load(Ordering::SeqCst) {
                break;
            }
            let visible = native::is_visible(hwnd)
                .unwrap_or_else(|| window.is_visible().unwrap_or(false));
            if !visible {
                continue;
            }
            let Some(cur) = current_cursor(&window) else {
                continue;
            };
            let Some(win) = current_window_rect(&window, hwnd) else {
                continue;
            };
            let m = DockMetrics::new(scale);
            let env = TickEnv {
                now: Instant::now(),
                cur,
                win,
                mouse_down: native::primary_button_down(),
                escape_down: native::escape_down(),
                double_press,
            };
            let action = {
                let mut st = dock.lock().unwrap();
                if st.animating {
                    continue;
                }
                decide_tick(&mut st, &env, &m, &mon)
            };
            apply_watcher_action(&window, &dock, &m, action);
        }
    });
    let _ = waker.set(handle.thread().clone());
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
    let hwnd = dock.inner.lock().unwrap().hwnd;
    let live_rect = current_window_rect(&window, hwnd);

    let rect_opt = {
        let mut st = dock.inner.lock().unwrap();
        let m = DockMetrics::new(st.scale);
        let clamped_logical = logical_h.clamp(POD_BASE_HEIGHT_LOGICAL_PX, 960.0);
        let new_h = (clamped_logical * m.scale).round() as i32;
        if st.tucked || st.orb_collapsed {
            None
        } else {
            let pod_w = st.pod_w_phys.max(m.pod_w);
            if collapsed {
                st.pod_h_phys = m.pod_h;
            }
            if st.custom_dragging {
                // Mid-drag: resize in place, never snap back to the docked position.
                live_rect.map(|(x, y, _, _)| (x, y, pod_w, new_h))
            } else {
                let (vis_x, vis_y) = compute_visible_pod_pos(
                    st.dock_x,
                    st.dock_y,
                    pod_w,
                    new_h,
                    st.work_area,
                    st.edge,
                    m.margin,
                );
                Some((vis_x, vis_y, pod_w, new_h))
            }
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
    dock.wake();
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
        // A deliberate choice of standby form replaces whatever play mode would restore.
        st.pre_play_standby = None;
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
    dock.wake();
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
    {
        let mut st = dock.inner.lock().unwrap();
        if st.orb_collapsed || st.tucked {
            st.force_untuck = true;
        }
    }
    dock.wake();
}

/// Starts dragging the capsule. `off_x`/`off_y` are where the press landed inside the
/// window, in logical pixels, so the grab point stays exactly under the cursor.
#[tauri::command]
fn begin_window_drag(
    window: WebviewWindow,
    dock: State<'_, DockController>,
    off_x: f64,
    off_y: f64,
) {
    if !is_primary_mouse_down() || !off_x.is_finite() || !off_y.is_finite() {
        return;
    }
    let hwnd = dock.inner.lock().unwrap().hwnd;
    let Some((wx, wy, ww, wh)) = current_window_rect(&window, hwnd) else {
        return;
    };
    let now = Instant::now();
    {
        let mut st = dock.inner.lock().unwrap();
        if st.orb_collapsed || st.tucked || st.animating {
            return;
        }
        let grab = (
            ((off_x * st.scale).round() as i32).clamp(0, ww.max(1)),
            ((off_y * st.scale).round() as i32).clamp(0, wh.max(1)),
        );
        let t = now.duration_since(st.epoch).as_secs_f64();
        st.custom_dragging = true;
        st.drag_offset = grab;
        st.vel_tracker.clear();
        st.vel_tracker.push(t, wx as f64, wy as f64);
        st.last_hover_at = now;
    }
    dock.wake();
}

/// The primary button went down on the ball. `off_x`/`off_y` are the press position inside
/// the window in logical pixels; `sent_at` is the page's wall-clock time in milliseconds,
/// used to drop a command that arrives after the press it describes is long over.
#[tauri::command]
fn orb_pointer_down(
    window: WebviewWindow,
    dock: State<'_, DockController>,
    off_x: f64,
    off_y: f64,
    sent_at: f64,
) {
    let lag = unix_millis_now() - sent_at;
    if !lag.is_finite() || !(-2000.0..=ORB_PRESS_MAX_IPC_LAG_MS).contains(&lag) {
        return;
    }
    if !off_x.is_finite() || !off_y.is_finite() {
        return;
    }
    let Some(cur) = current_cursor(&window) else {
        return;
    };
    let hwnd = dock.inner.lock().unwrap().hwnd;
    let Some((wx, wy, _, _)) = current_window_rect(&window, hwnd) else {
        return;
    };
    let now = Instant::now();
    let entered_payload = {
        let mut st = dock.inner.lock().unwrap();
        let m = DockMetrics::new(st.scale);
        let outcome = orb_press_begin(
            &mut st,
            now,
            (cur.0.round() as i32, cur.1.round() as i32),
            (wx, wy),
            (off_x, off_y),
            &m,
        );
        if outcome == PressOutcome::EnteredPlay {
            Some(st.payload())
        } else {
            None
        }
    };
    if let Some(payload) = entered_payload {
        let _ = window.emit("dock-state", payload);
    }
    dock.wake();
}

/// The button came up. The watcher notices this on its own by polling; this only wakes it
/// so the release is handled without waiting for the next tick.
#[tauri::command]
fn orb_pointer_up(dock: State<'_, DockController>) {
    dock.wake();
}

/// Leaves play mode. Used by the close badge, right click and Escape.
#[tauri::command]
fn exit_orb_play(dock: State<'_, DockController>) {
    {
        let mut st = dock.inner.lock().unwrap();
        let m = DockMetrics::new(st.scale);
        request_exit_play(&mut st, Instant::now(), &m);
    }
    dock.wake();
}

/// Enters play mode from the capsule (double click on its ring), or toggles it when the
/// ball is already showing.
#[tauri::command]
fn toggle_orb_play_mode(
    window: WebviewWindow,
    dock: State<'_, DockController>,
) -> DockStatePayload {
    let cursor = current_cursor(&window).map(|c| (c.0.round() as i32, c.1.round() as i32));
    let hwnd = dock.inner.lock().unwrap().hwnd;
    let win_pos = current_window_rect(&window, hwnd).map(|(x, y, _, _)| (x, y));
    let now = Instant::now();

    let (payload, emit_now) = {
        let mut st = dock.inner.lock().unwrap();
        let m = DockMetrics::new(st.scale);
        let mut emit_now = false;
        if st.animating || now < st.toggle_lock_until {
            // Ignore repeats that arrive while a toggle is still settling.
        } else if st.play_mode {
            request_exit_play(&mut st, now, &m);
        } else if st.orb_collapsed {
            let start = win_pos.unwrap_or_else(|| orb_home(&mut st, &m));
            enter_play_from_orb(&mut st, now, start, &m);
            emit_now = true;
        } else if !st.tucked {
            enter_play_from_pod(&mut st, now, cursor, &m);
        }
        (st.payload(), emit_now)
    };
    if emit_now {
        let _ = window.emit("dock-state", payload.clone());
    }
    dock.wake();
    payload
}

#[tauri::command]
fn tuck_or_hide(app: AppHandle, dock: State<'_, DockController>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let should_hide = {
        let mut st = dock.inner.lock().unwrap();
        let m = DockMetrics::new(st.scale);
        request_tuck(&mut st, Instant::now(), &m)
    };
    dock.wake();
    if should_hide {
        let _ = window.hide();
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let mut immediate_payload: Option<DockStatePayload> = None;
        let dock = app.state::<DockController>();
        {
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
        dock.wake();
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
    let dock_waker: Arc<OnceLock<std::thread::Thread>> = Arc::new(OnceLock::new());

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
            waker: Arc::clone(&dock_waker),
        })
        .setup({
            let quitting = Arc::clone(&quitting);
            let dock_inner = Arc::clone(&dock_inner);
            let dock_waker = Arc::clone(&dock_waker);
            move |app| {
                setup_tray(app.handle())?;
                if let Some(win) = app.get_webview_window("main") {
                    init_window_dock(&win, &dock_inner);
                    start_dock_watcher(win, dock_inner, dock_waker, quitting);
                }
                Ok(())
            }
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                match event {
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        let dock = window.app_handle().state::<DockController>();
                        dock.inner.lock().unwrap().scale = *scale_factor;
                    }
                    WindowEvent::Moved(pos) => {
                        let dock = window.app_handle().state::<DockController>();
                        let now = Instant::now();
                        let mut untuck = false;
                        {
                            let mut st = dock.inner.lock().unwrap();
                            // Every move we make ourselves is announced first. While a drag,
                            // a press on the ball or play mode is running, all moves are ours.
                            let is_prog = st.animating
                                || now < st.ignore_moves_until
                                || st.last_programmatic_target == Some((pos.x, pos.y))
                                || st.custom_dragging
                                || st.orb_press.is_some()
                                || st.pending_tap.is_some()
                                || st.play_mode
                                || st.orb_coasting;
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
            orb_pointer_down,
            orb_pointer_up,
            exit_orb_play,
            toggle_orb_play_mode,
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
    fn progress_lines_are_recognised_and_other_output_is_not() {
        assert_eq!(
            parse_progress_line("PROGRESS:download:42\r\n"),
            Some(("download".to_string(), 42))
        );
        assert_eq!(
            parse_progress_line("PROGRESS:install:250"),
            Some(("install".to_string(), 100))
        );
        assert_eq!(parse_progress_line("OK:已通过完整性校验"), None);
        assert_eq!(parse_progress_line("PROGRESS:download:"), None);
        assert_eq!(parse_progress_line("PROGRESS:bad stage:5"), None);
        assert_eq!(parse_progress_line("  INFO: PROGRESS:download:5"), None);
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

    // ---- Ball gesture state machine, driven tick by tick without a desktop ----

    struct FakeMonitors(WorkRect);

    impl MonitorProbe for FakeMonitors {
        fn work_rect_at(&self, _x: i32, _y: i32) -> Option<WorkRect> {
            Some(self.0)
        }
        fn exists_at(&self, _x: i32, _y: i32) -> bool {
            false
        }
    }

    const TEST_AREA: WorkRect = WorkRect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1040,
    };
    const ORB_HOME: (i32, i32) = (800, 400);
    const ORB_WIN: (i32, i32, i32, i32) = (800, 400, 72, 72);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A dock parked as the ball at ORB_HOME in "orb" standby. The returned instant is late
    /// enough that every startup grace period is already over.
    fn parked_orb() -> (DockInner, Instant) {
        let mut st = DockInner::new();
        st.work_area = TEST_AREA;
        st.standby_mode = StandbyMode::Orb;
        st.edge = DockEdge::None;
        st.dock_x = 596;
        st.dock_y = 404;
        st.orb_x = Some(ORB_HOME.0);
        st.orb_y = Some(ORB_HOME.1);
        st.orb_collapsed = true;
        (st, Instant::now() + ms(5000))
    }

    fn tick_env(
        now: Instant,
        cur: (f64, f64),
        win: (i32, i32, i32, i32),
        mouse_down: bool,
    ) -> TickEnv {
        TickEnv {
            now,
            cur,
            win,
            mouse_down,
            escape_down: false,
            double_press: ms(275),
        }
    }

    /// Applies the part of an action that moves or reshapes the window, as the watcher would.
    fn follow(st: &mut DockInner, win: &mut (i32, i32, i32, i32), action: &WatcherAction) {
        match action {
            WatcherAction::MoveFast { to, .. }
            | WatcherAction::SaveOrbDrag { to, .. }
            | WatcherAction::GlideOrbReturn { to, .. } => {
                win.0 = to.0;
                win.1 = to.1;
            }
            WatcherAction::EnterOrb {
                orb_pos, orb_size, ..
            } => {
                st.orb_collapsed = true;
                *win = (orb_pos.0, orb_pos.1, *orb_size, *orb_size);
            }
            WatcherAction::ExitOrb {
                pod_pos,
                pod_w,
                pod_h,
                ..
            } => {
                *win = (pod_pos.0, pod_pos.1, *pod_w, *pod_h);
            }
            _ => {}
        }
    }

    fn center_of(win: (i32, i32, i32, i32)) -> (f64, f64) {
        (
            win.0 as f64 + win.2 as f64 / 2.0,
            win.1 as f64 + win.3 as f64 / 2.0,
        )
    }

    fn press_center(
        st: &mut DockInner,
        m: &DockMetrics,
        at: Instant,
        win: (i32, i32, i32, i32),
    ) -> PressOutcome {
        let c = center_of(win);
        orb_press_begin(
            st,
            at,
            (c.0 as i32, c.1 as i32),
            (win.0, win.1),
            (36.0, 36.0),
            m,
        )
    }

    /// Presses and releases on the middle of the ball. Returns the time of the release tick.
    fn tap(
        st: &mut DockInner,
        m: &DockMetrics,
        mon: &FakeMonitors,
        at: Instant,
        win: &mut (i32, i32, i32, i32),
    ) -> Instant {
        assert_eq!(press_center(st, m, at, *win), PressOutcome::Pressed);
        let c = center_of(*win);
        let held = decide_tick(st, &tick_env(at + ms(16), c, *win, true), m, mon);
        follow(st, win, &held);
        let released_at = at + ms(70);
        let released = decide_tick(st, &tick_env(released_at, c, *win, false), m, mon);
        follow(st, win, &released);
        released_at
    }

    /// Runs physics ticks with the cursor parked far away until the ball comes to rest.
    fn settle(
        st: &mut DockInner,
        m: &DockMetrics,
        mon: &FakeMonitors,
        from: Instant,
        win: &mut (i32, i32, i32, i32),
    ) -> Instant {
        let mut t = from;
        for _ in 0..2500 {
            t += ms(8);
            let action = decide_tick(st, &tick_env(t, (10.0, 10.0), *win, false), m, mon);
            follow(st, win, &action);
            if st.physics.sleeping {
                break;
            }
        }
        t
    }

    fn resting_play_ball(
        m: &DockMetrics,
        mon: &FakeMonitors,
    ) -> (DockInner, Instant, (i32, i32, i32, i32)) {
        let (mut st, t0) = parked_orb();
        let mut win = ORB_WIN;
        enter_play_from_orb(&mut st, t0, (win.0, win.1), m);
        let rested = settle(&mut st, m, mon, t0, &mut win);
        assert!(st.play_mode && st.physics.sleeping);
        assert_eq!(win.1, 1040 - 72 + 8, "the ball rests on the bottom of the work area");
        (st, rested + ms(600), win)
    }

    #[test]
    fn single_tap_on_parked_ball_opens_capsule_after_double_press_window() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t0) = parked_orb();
        let mut win = ORB_WIN;
        let c = center_of(win);

        let released = tap(&mut st, &m, &mon, t0, &mut win);
        assert!(st.pending_tap.is_some());
        assert!(st.orb_collapsed, "a tap must wait for a possible second press");

        let waiting = decide_tick(&mut st, &tick_env(released + ms(150), c, win, false), &m, &mon);
        assert!(matches!(waiting, WatcherAction::None));
        assert!(st.orb_collapsed);

        let opened = decide_tick(&mut st, &tick_env(released + ms(280), c, win, false), &m, &mon);
        match opened {
            WatcherAction::ExitOrb { guard_clicks, .. } => assert!(guard_clicks),
            _ => panic!("a single tap should open the capsule"),
        }
        assert!(!st.orb_collapsed);
        assert!(!st.play_mode);
    }

    #[test]
    fn double_press_enters_play_and_a_second_double_press_flies_the_ball_home() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t0) = parked_orb();
        let mut win = ORB_WIN;

        let released = tap(&mut st, &m, &mon, t0, &mut win);
        assert_eq!(
            press_center(&mut st, &m, released + ms(110), win),
            PressOutcome::EnteredPlay
        );
        assert!(st.play_mode && st.orb_collapsed);
        assert!(st.pending_tap.is_none());
        assert_eq!(st.pre_play_orb_pos, Some(ORB_HOME));

        // A third click right behind the double click must not toggle straight back.
        assert_eq!(
            press_center(&mut st, &m, released + ms(260), win),
            PressOutcome::Ignored
        );
        assert!(st.play_mode);

        let rested = settle(&mut st, &m, &mon, released + ms(300), &mut win);
        assert!(st.physics.sleeping && st.play_mode);
        assert_eq!(win.1, 1040 - 72 + 8);

        // Double press on the resting ball: the first press holds it still so the second lands.
        let before = (win.0, win.1);
        let released2 = tap(&mut st, &m, &mon, rested + ms(500), &mut win);
        let c = center_of(win);
        let frozen = decide_tick(&mut st, &tick_env(released2 + ms(100), c, win, false), &m, &mon);
        assert!(matches!(frozen, WatcherAction::None));
        assert_eq!((win.0, win.1), before, "the ball holds still between the two presses");

        assert_eq!(
            press_center(&mut st, &m, released2 + ms(140), win),
            PressOutcome::ExitedPlay
        );
        assert!(!st.play_mode);

        let fly = decide_tick(&mut st, &tick_env(released2 + ms(148), c, win, true), &m, &mon);
        match fly {
            WatcherAction::GlideOrbReturn { to, payload, .. } => {
                assert_eq!(to, ORB_HOME);
                assert!(payload.orb && !payload.play);
            }
            _ => panic!("leaving play mode should fly the ball home"),
        }
        assert!(st.pending_orb_return.is_none());
        assert_eq!(st.work_area, TEST_AREA);
    }

    #[test]
    fn single_tap_in_play_holds_the_ball_then_pops_it_upward() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t, mut win) = resting_play_ball(&m, &mon);
        let before = (win.0, win.1);

        let released = tap(&mut st, &m, &mon, t, &mut win);
        assert!(st.pending_tap.is_some());
        let c = center_of(win);

        // While it waits, neither gravity nor a cursor racing past moves the ball.
        let sweep = decide_tick(
            &mut st,
            &tick_env(released + ms(60), (c.0 + 400.0, c.1), win, false),
            &m,
            &mon,
        );
        assert!(matches!(sweep, WatcherAction::None));
        assert_eq!((win.0, win.1), before);

        let pop = decide_tick(&mut st, &tick_env(released + ms(280), c, win, false), &m, &mon);
        assert!(matches!(pop, WatcherAction::EmitOrbFx(_)));
        assert!(st.physics.vy < -500.0 && !st.physics.sleeping);
        assert!(st.play_mode);
    }

    #[test]
    fn pressing_a_flying_ball_catches_it_and_it_follows_the_cursor() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t0) = parked_orb();
        let mut win = ORB_WIN;
        enter_play_from_orb(&mut st, t0, (win.0, win.1), &m);

        let mut t = t0;
        for _ in 0..50 {
            t += ms(8);
            let action = decide_tick(&mut st, &tick_env(t, (10.0, 10.0), win, false), &m, &mon);
            follow(&mut st, &mut win, &action);
        }
        assert!(!st.physics.sleeping, "the ball should still be in the air");

        let c = center_of(win);
        assert_eq!(press_center(&mut st, &m, t, win), PressOutcome::Pressed);
        assert_eq!((st.physics.vx, st.physics.vy), (0.0, 0.0));

        // Held with a still cursor: no gravity, the ball stays under the cursor.
        let held_pos = (win.0, win.1);
        for i in 1..=10 {
            let action = decide_tick(&mut st, &tick_env(t + ms(8 * i), c, win, true), &m, &mon);
            follow(&mut st, &mut win, &action);
        }
        assert_eq!((win.0, win.1), held_pos);

        // Moving the cursor carries the ball, keeping the grab point under it.
        let moved = (c.0 + 120.0, c.1 - 80.0);
        let action = decide_tick(&mut st, &tick_env(t + ms(96), moved, win, true), &m, &mon);
        follow(&mut st, &mut win, &action);
        assert_eq!((win.0, win.1), (held_pos.0 + 120, held_pos.1 - 80));
    }

    #[test]
    fn hover_opens_the_capsule_only_after_a_calm_dwell() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t0) = parked_orb();
        let win = ORB_WIN;
        let c = center_of(win);

        let away = decide_tick(&mut st, &tick_env(t0, (c.0 + 200.0, c.1), win, false), &m, &mon);
        assert!(matches!(away, WatcherAction::None));
        assert!(st.orb_hover_armed);

        // A fast sweep across the ball never counts as hovering.
        for (i, dx) in [-60.0, 0.0, 60.0].iter().enumerate() {
            let at = t0 + ms(24 * (i as u64 + 1));
            let action = decide_tick(&mut st, &tick_env(at, (c.0 + dx, c.1), win, false), &m, &mon);
            assert!(matches!(action, WatcherAction::None));
        }

        // Resting on the ball: nothing happens before the dwell time is over.
        let arrive = t0 + ms(200);
        for i in 0..=15u64 {
            let action = decide_tick(&mut st, &tick_env(arrive + ms(24 * i), c, win, false), &m, &mon);
            assert!(matches!(action, WatcherAction::None), "opened too early at step {i}");
        }
        assert!(st.orb_collapsed);

        let opened = decide_tick(&mut st, &tick_env(arrive + ms(24 * 16), c, win, false), &m, &mon);
        match opened {
            WatcherAction::ExitOrb { guard_clicks, .. } => assert!(guard_clicks),
            _ => panic!("hover should open the capsule after the dwell"),
        }
    }

    #[test]
    fn hover_never_opens_the_capsule_under_a_held_button() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t0) = parked_orb();
        let win = ORB_WIN;
        let c = center_of(win);
        let _ = decide_tick(&mut st, &tick_env(t0, (c.0 + 200.0, c.1), win, false), &m, &mon);

        // The button is held on the ball well past the dwell time.
        for i in 1..=30u64 {
            let action = decide_tick(&mut st, &tick_env(t0 + ms(24 * i), c, win, true), &m, &mon);
            assert!(matches!(action, WatcherAction::None));
        }
        assert!(st.orb_collapsed);

        // Right after release it still holds off for a moment.
        let released = t0 + ms(24 * 31);
        let soon = decide_tick(&mut st, &tick_env(released + ms(200), c, win, false), &m, &mon);
        assert!(matches!(soon, WatcherAction::None));
        assert!(st.orb_collapsed);
    }

    #[test]
    fn dropping_the_ball_parks_it_where_released_and_only_a_flick_makes_it_glide() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);

        // A normal placement at about 500 px/s.
        let (mut st, t0) = parked_orb();
        let mut win = ORB_WIN;
        let c = center_of(win);
        assert_eq!(press_center(&mut st, &m, t0, win), PressOutcome::Pressed);
        let nudge = decide_tick(&mut st, &tick_env(t0 + ms(8), (c.0 + 3.0, c.1), win, true), &m, &mon);
        assert!(matches!(nudge, WatcherAction::None), "movement inside the slop is not a drag");

        let mut t = t0 + ms(8);
        let mut cur = c;
        for _ in 0..75 {
            t += ms(8);
            cur.0 += 4.0;
            let action = decide_tick(&mut st, &tick_env(t, cur, win, true), &m, &mon);
            follow(&mut st, &mut win, &action);
        }
        assert_eq!(win.0, ORB_HOME.0 + 300);

        let drop = decide_tick(&mut st, &tick_env(t + ms(8), cur, win, false), &m, &mon);
        match drop {
            WatcherAction::SaveOrbDrag { to, .. } => assert_eq!(to, (ORB_HOME.0 + 300, ORB_HOME.1)),
            _ => panic!("a slow drop should park the ball right where it was released"),
        }
        assert!(!st.orb_coasting && st.orb_press.is_none());
        assert_eq!(st.orb_x, Some(ORB_HOME.0 + 300));

        // A deliberate flick at about 2500 px/s.
        let (mut st, t0) = parked_orb();
        let mut win = ORB_WIN;
        assert_eq!(press_center(&mut st, &m, t0, win), PressOutcome::Pressed);
        let mut t = t0;
        let mut cur = center_of(win);
        for _ in 0..12 {
            t += ms(8);
            cur.0 += 20.0;
            let action = decide_tick(&mut st, &tick_env(t, cur, win, true), &m, &mon);
            follow(&mut st, &mut win, &action);
        }
        let flick = decide_tick(&mut st, &tick_env(t + ms(8), cur, win, false), &m, &mon);
        assert!(matches!(flick, WatcherAction::None));
        assert!(st.orb_coasting, "a flick should let the ball glide on");
    }

    #[test]
    fn reaching_for_a_resting_ball_does_not_kick_it_but_a_fast_swipe_does() {
        let bounds = OrbPhysicsBounds::from_work_area(TEST_AREA, 72, 8);
        let mut phys = OrbPhysics {
            px: 600.0,
            py: bounds.max_y,
            vx: 0.0,
            vy: 0.0,
            omega: 0.0,
            angle_deg: 0.0,
            sleeping: true,
            combo: 0,
        };
        let ball = (636.0, bounds.max_y + 36.0);

        // Ordinary pointing speed, about 800 px/s, arriving on the ball.
        let calm = apply_cursor_kick(
            &mut phys,
            (ball.0 - 40.0, ball.1),
            (ball.0 - 27.0, ball.1),
            0.016,
            72.0,
            bounds,
            1.0,
        );
        assert!(calm.is_none());
        assert!(phys.sleeping);

        // A deliberate swipe, about 2500 px/s.
        let swipe = apply_cursor_kick(
            &mut phys,
            (ball.0 - 50.0, ball.1 + 10.0),
            (ball.0 - 10.0, ball.1 + 10.0),
            0.016,
            72.0,
            bounds,
            1.0,
        );
        assert!(swipe.is_some());
        assert!(!phys.sleeping && phys.vy < 0.0);
    }

    #[test]
    fn escape_leaves_play_mode_once_and_never_latches_a_collapse() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t, _win) = resting_play_ball(&m, &mon);

        // Escape arriving through the collapse command ends play mode instead of collapsing.
        assert!(!request_tuck(&mut st, t, &m));
        assert!(!st.play_mode);
        assert_eq!(st.pending_orb_return, Some(ORB_HOME));
        assert!(!st.force_tuck);

        // The same key press arriving a second time does nothing.
        assert!(!request_tuck(&mut st, t + ms(40), &m));
        assert!(!st.force_tuck);

        // A collapse request while already collapsed must not stay latched for later.
        let (mut parked, t0) = parked_orb();
        assert!(!request_tuck(&mut parked, t0, &m));
        assert!(!parked.force_tuck);
    }

    #[test]
    fn polled_escape_exits_play_only_when_the_cursor_is_near_the_ball() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t, mut win) = resting_play_ball(&m, &mon);
        let c = center_of(win);

        let mut typed_elsewhere = tick_env(t, (c.0 - 600.0, c.1 - 500.0), win, false);
        typed_elsewhere.escape_down = true;
        let action = decide_tick(&mut st, &typed_elsewhere, &m, &mon);
        follow(&mut st, &mut win, &action);
        assert!(st.play_mode, "Escape typed elsewhere must not end play mode");

        // Key released, cursor strolls over to the ball, key pressed again.
        let action = decide_tick(&mut st, &tick_env(t + ms(2000), c, win, false), &m, &mon);
        follow(&mut st, &mut win, &action);
        let mut near = tick_env(t + ms(2016), c, win, false);
        near.escape_down = true;
        let _ = decide_tick(&mut st, &near, &m, &mon);
        assert!(!st.play_mode);
        assert_eq!(st.pending_orb_return, Some(ORB_HOME));
    }

    #[test]
    fn a_resting_ball_goes_home_by_itself_after_the_idle_timeout() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t, win) = resting_play_ball(&m, &mon);

        let _ = decide_tick(
            &mut st,
            &tick_env(t + Duration::from_secs(20), (10.0, 10.0), win, false),
            &m,
            &mon,
        );
        assert!(st.play_mode);

        let _ = decide_tick(
            &mut st,
            &tick_env(t + Duration::from_secs(60), (10.0, 10.0), win, false),
            &m,
            &mon,
        );
        assert!(!st.play_mode);
        assert_eq!(st.pending_orb_return, Some(ORB_HOME));
    }

    #[test]
    fn play_started_from_the_capsule_restores_the_capsule_and_its_standby_form() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let capsule = || {
            let mut st = DockInner::new();
            st.work_area = TEST_AREA;
            st.standby_mode = StandbyMode::Edge;
            st.edge = DockEdge::Top;
            st.dock_x = 720;
            st.dock_y = 0;
            st
        };
        let t0 = Instant::now() + ms(5000);

        // Cancelled before the capsule even collapsed: it simply stays a capsule.
        let mut st = capsule();
        enter_play_from_pod(&mut st, t0, Some((900, 30)), &m);
        assert!(st.play_mode && st.force_tuck);
        request_exit_play(&mut st, t0 + ms(4), &m);
        assert!(!st.play_mode && !st.force_tuck && !st.force_untuck);
        assert_eq!(st.standby_mode, StandbyMode::Edge);

        // Played, then left: the capsule comes back, still in edge standby.
        let mut st = capsule();
        let mut win = (720, 0, 480, 64);
        enter_play_from_pod(&mut st, t0, Some((900, 30)), &m);
        let collapse = decide_tick(&mut st, &tick_env(t0 + ms(8), (900.0, 30.0), win, false), &m, &mon);
        assert!(matches!(collapse, WatcherAction::EnterOrb { play: true, .. }));
        follow(&mut st, &mut win, &collapse);

        request_exit_play(&mut st, t0 + ms(900), &m);
        assert!(!st.play_mode);
        assert_eq!(st.standby_mode, StandbyMode::Edge);
        let reopen = decide_tick(&mut st, &tick_env(t0 + ms(908), (900.0, 30.0), win, false), &m, &mon);
        assert!(matches!(
            reopen,
            WatcherAction::ExitOrb {
                guard_clicks: false,
                ..
            }
        ));
        assert!(!st.orb_collapsed);
    }

    #[test]
    fn a_stale_press_far_from_the_ball_is_ignored() {
        let m = DockMetrics::new(1.0);
        let (mut st, t0) = parked_orb();
        // The cursor is 500px from where this press claims to have landed.
        let outcome = orb_press_begin(&mut st, t0, (1336, 436), ORB_HOME, (36.0, 36.0), &m);
        assert_eq!(outcome, PressOutcome::Ignored);
        assert!(st.orb_press.is_none());
    }
}
