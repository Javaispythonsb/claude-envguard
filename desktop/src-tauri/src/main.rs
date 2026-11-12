#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

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
    quitting: AtomicBool,
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

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
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
    let open_console = MenuItemBuilder::with_id("open_console", "打开主控制台").build(app)?;
    let launch_desktop =
        MenuItemBuilder::with_id("launch_desktop", "▶ 安全启动 Claude").build(app)?;
    let launch_cli = MenuItemBuilder::with_id("launch_cli", "❯_ 安全启动 CLI").build(app)?;
    let recheck = MenuItemBuilder::with_id("recheck", "↻ 重新检查环境").build(app)?;
    let shield_restore =
        MenuItemBuilder::with_id("shield_restore", "退出护盾（还原网络并停保镖）").build(app)?;
    let quit_app =
        MenuItemBuilder::with_id("quit_app", "退出控制台（保留后台保镖）").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&open_console)
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
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .manage(OpGuard {
            busy: Arc::new(AtomicBool::new(false)),
            quitting: AtomicBool::new(false),
        })
        .setup(|app| {
            setup_tray(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
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
            export_diagnostics
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
}
