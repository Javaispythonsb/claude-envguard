use std::path::Path;
use std::process::Command;

fn git_stdout(repo_root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let repo_root = Path::new(&manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("."));

    let git_dir = repo_root.join(".git");
    if git_dir.exists() {
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join("refs/heads/main").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join("packed-refs").display()
        );
    }
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    let env_sha = std::env::var("GITHUB_SHA")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()));

    let full_sha = env_sha
        .or_else(|| git_stdout(repo_root, &["rev-parse", "HEAD"]))
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "unknown".to_string());

    let short_sha = if full_sha != "unknown" && full_sha.len() >= 7 {
        full_sha[..7].to_string()
    } else {
        "unknown".to_string()
    };

    let commit_unix = git_stdout(repo_root, &["show", "-s", "--format=%ct", "HEAD"])
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);

    println!("cargo:rustc-env=ENVGUARD_GIT_SHA={full_sha}");
    println!("cargo:rustc-env=ENVGUARD_GIT_SHORT_SHA={short_sha}");
    println!("cargo:rustc-env=ENVGUARD_GIT_COMMIT_UNIX={commit_unix}");

    tauri_build::build()
}
