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

/// Saved capsule placement. Files written by older versions may also carry
/// `standby_mode`, `orb_x` and `orb_y`; serde ignores those fields.
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

struct DockInner {
    epoch: Instant,
    hwnd: isize,
    scale: f64,
    edge: DockEdge,
    dock_x: i32,
    dock_y: i32,
    pod_w_phys: i32,
    pod_h_phys: i32,
    work_area: WorkRect,
    tucked: bool,
    custom_dragging: bool,
    drag_offset: (i32, i32),
    vel_tracker: VelocityTracker,
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
            dock_x: 0,
            dock_y: 0,
            pod_w_phys: 480,
            pod_h_phys: 64,
            work_area: WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            tucked: false,
            custom_dragging: false,
            drag_offset: (24, 24),
            vel_tracker: VelocityTracker::default(),
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

    fn to_saved(&self) -> SavedDock {
        SavedDock {
            x: self.dock_x,
            y: self.dock_y,
            edge: self.edge,
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
    }

    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const MONITOR_DEFAULTTONULL: u32 = 0;
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    const VK_LBUTTON: i32 = 0x01;
    const VK_RBUTTON: i32 = 0x02;
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
}

fn is_primary_mouse_down() -> bool {
    native::primary_button_down()
}

const PEEK_LOGICAL_PX: f64 = 9.0;
const SNAP_THRESHOLD_LOGICAL_PX: f64 = 48.0;
const POD_WIDTH_LOGICAL_PX: f64 = 480.0;
const POD_BASE_HEIGHT_LOGICAL_PX: f64 = 64.0;

/// Pixel sizes derived from the current DPI scale.
#[derive(Clone, Copy, Debug)]
struct DockMetrics {
    scale: f64,
    peek: i32,
    threshold: i32,
    margin: i32,
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
            st.custom_dragging = false;
            st.vel_tracker.clear();
            st.scale = scale;
            st.edge = DockEdge::Top;
            st.dock_x = x;
            st.dock_y = y;
            st.pod_w_phys = pod_w;
            st.pod_h_phys = pod_h;
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
                    st.dock_x = x;
                    st.dock_y = y;
                    st.pod_w_phys = ww;
                    st.pod_h_phys = wh;
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
    MoveFast {
        to: (i32, i32),
        payload: Option<DockStatePayload>,
    },
    SnapPod {
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
    EmitOnly(DockStatePayload),
}

/// Everything one watcher tick reads from the outside world.
struct TickEnv {
    now: Instant,
    cur: (f64, f64),
    win: (i32, i32, i32, i32),
    mouse_down: bool,
}

impl TickEnv {
    fn cursor_px(&self) -> (i32, i32) {
        (self.cur.0.round() as i32, self.cur.1.round() as i32)
    }
}

/// Collapse request from Escape or the hide button. Returns true when the window should
/// be hidden to the tray instead, which is what happens when it floats free of any edge.
fn request_tuck(st: &mut DockInner) -> bool {
    if st.tucked {
        return false;
    }
    if st.edge != DockEdge::None {
        st.force_tuck = true;
        return false;
    }
    true
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
    st.tucked = false;
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

    if !st.tucked && ww >= (m.pod_w - 20) {
        st.pod_w_phys = ww;
        if wh <= m.pod_h + (24.0 * m.scale).round() as i32 {
            st.pod_h_phys = wh;
        }
    }

    // 1. The capsule is being dragged.
    if st.custom_dragging {
        if st.tucked {
            st.custom_dragging = false;
        } else {
            return decide_pod_drag(st, env, m, mon);
        }
    }

    // 2. Something asked for the capsule to be shown.
    if st.force_untuck {
        st.force_untuck = false;
        if st.tucked {
            st.force_tuck = false;
            st.tucked = false;
            st.idle = false;
            st.last_hover_at = now;
            st.grace_until = now + Duration::from_millis(2400);
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
        if !st.tucked && st.edge != DockEdge::None {
            st.pinned = false;
            st.ui_hold = false;
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

    // 4. The window was moved by the system rather than by us.
    if st.pending_user_move {
        return decide_os_move(st, env, m, mon);
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
    if st.custom_dragging || st.force_tuck || st.force_untuck {
        8
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
        WatcherAction::MoveFast { to, payload } => {
            set_window_pos_fast(window, dock, to.0, to.1);
            if let Some(p) = payload {
                let _ = window.emit("dock-state", p);
            }
        }
        WatcherAction::EmitOnly(payload) => {
            let _ = window.emit("dock-state", payload);
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
        if st.tucked {
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
    dock.wake();
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
        if st.tucked || st.animating {
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

#[tauri::command]
fn tuck_or_hide(app: AppHandle, dock: State<'_, DockController>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let should_hide = {
        let mut st = dock.inner.lock().unwrap();
        request_tuck(&mut st)
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
            if st.tucked {
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
                            // Every move we make ourselves is announced first, and during a
                            // drag all moves are ours.
                            let is_prog = st.animating
                                || now < st.ignore_moves_until
                                || st.last_programmatic_target == Some((pos.x, pos.y))
                                || st.custom_dragging;
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
            begin_window_drag,
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

    // ---- Capsule dock state machine, driven tick by tick without a desktop ----

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

    fn capsule_at(edge: DockEdge, x: i32, y: i32) -> (DockInner, Instant) {
        let mut st = DockInner::new();
        st.work_area = TEST_AREA;
        st.edge = edge;
        st.dock_x = x;
        st.dock_y = y;
        (st, Instant::now() + Duration::from_millis(5000))
    }

    fn env_at(now: Instant, cur: (f64, f64), win: (i32, i32, i32, i32), mouse_down: bool) -> TickEnv {
        TickEnv {
            now,
            cur,
            win,
            mouse_down,
        }
    }

    #[test]
    fn hide_request_tucks_a_docked_capsule_and_sends_a_floating_one_to_the_tray() {
        let (mut docked, _) = capsule_at(DockEdge::Top, 720, 0);
        assert!(!request_tuck(&mut docked));
        assert!(docked.force_tuck);

        let (mut floating, _) = capsule_at(DockEdge::None, 600, 400);
        assert!(request_tuck(&mut floating));
        assert!(!floating.force_tuck);

        // Already tucked: nothing to do, and nothing left latched for later.
        let (mut tucked, _) = capsule_at(DockEdge::Right, 1440, 300);
        tucked.tucked = true;
        assert!(!request_tuck(&mut tucked));
        assert!(!tucked.force_tuck);
    }

    #[test]
    fn docked_capsule_tucks_after_the_cursor_leaves_and_reopens_from_the_light_bar() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t0) = capsule_at(DockEdge::Top, 720, 0);
        let win = (720, 0, 480, 64);

        let inside = decide_tick(&mut st, &env_at(t0, (900.0, 30.0), win, false), &m, &mon);
        assert!(matches!(inside, WatcherAction::None));

        let left = decide_tick(&mut st, &env_at(t0 + Duration::from_millis(1300), (900.0, 600.0), win, false), &m, &mon);
        assert!(matches!(left, WatcherAction::TuckEdge { edge: DockEdge::Top, .. }));

        st.tucked = true;
        let tucked_win = (720, -55, 480, 64);
        let back = decide_tick(&mut st, &env_at(t0 + Duration::from_millis(1400), (960.0, 2.0), tucked_win, false), &m, &mon);
        match back {
            WatcherAction::UntuckEdge { to, .. } => assert_eq!(to, (720, 0)),
            _ => panic!("hovering the light bar should bring the capsule back"),
        }
        assert!(!st.tucked);
    }

    #[test]
    fn dragging_the_capsule_follows_the_cursor_and_snaps_to_the_nearest_edge_on_release() {
        let m = DockMetrics::new(1.0);
        let mon = FakeMonitors(TEST_AREA);
        let (mut st, t0) = capsule_at(DockEdge::None, 600, 400);
        st.custom_dragging = true;
        st.drag_offset = (100, 30);

        let held = decide_tick(&mut st, &env_at(t0, (800.0, 60.0), (600, 400, 480, 64), true), &m, &mon);
        match held {
            WatcherAction::MoveFast { to, .. } => assert_eq!(to, (700, 30)),
            _ => panic!("the capsule should follow the cursor while dragged"),
        }
        assert_eq!(st.preview, DockEdge::Top);

        let dropped = decide_tick(&mut st, &env_at(t0 + Duration::from_millis(16), (800.0, 60.0), (700, 30, 480, 64), false), &m, &mon);
        match dropped {
            WatcherAction::SnapPod { to, saved, .. } => {
                assert_eq!(to.1, 0);
                assert_eq!(saved.edge, DockEdge::Top);
            }
            _ => panic!("releasing near the top should snap the capsule to it"),
        }
        assert!(!st.custom_dragging);
    }

    #[test]
    fn saved_dock_files_from_the_ball_era_still_load() {
        let raw = r#"{"x":1296,"y":504,"edge":"none","standby_mode":"orb","orb_x":1500,"orb_y":500}"#;
        let saved: SavedDock = serde_json::from_str(raw).expect("old window-dock.json must still parse");
        assert_eq!((saved.x, saved.y, saved.edge), (1296, 504, DockEdge::None));
    }
}
