use std::path::{Path, PathBuf};
use std::process::Command;

pub fn is_available() -> bool {
    which_fanficfare().is_some()
}

pub fn which_fanficfare() -> Option<PathBuf> {
    let candidates = ["fanficfare", "fanficfare.exe"];
    for c in candidates {
        if let Ok(paths) = std::env::var("PATH") {
            for dir in std::env::split_paths(&paths) {
                let p = dir.join(c);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    // Try `which` as fallback.
    for c in candidates {
        if let Ok(out) = Command::new("which").arg(c).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(PathBuf::from(s));
                }
            }
        }
    }
    None
}

/// Download `url` into `staging_dir` (must exist). Returns list of produced .epub files.
pub fn download_url(url: &str, staging_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let before: std::collections::HashSet<String> = list_epubs(staging_dir)
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let mut cmd = Command::new("fanficfare");
    cmd.arg("--non-interactive")
        .arg(url)
        .current_dir(staging_dir);
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run fanficfare: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Err(format!(
            "fanficfare failed: {}\n{}",
            stdout.trim(),
            stderr.trim()
        ));
    }
    let after = list_epubs(staging_dir);
    let mut fresh: Vec<PathBuf> = after
        .into_iter()
        .filter(|p| !before.contains(&p.to_string_lossy().to_string()))
        .collect();
    if fresh.is_empty() {
        // FanFicFare may reuse a name that already existed; fall back to newest epub.
        fresh = newest_epubs(staging_dir, 1);
    }
    if fresh.is_empty() {
        return Err("fanficfare produced no .epub file".to_string());
    }
    Ok(fresh)
}

/// Update an existing epub file in place. Runs `fanficfare -u <file>`.
pub fn update_epub_file(epub_path: &Path) -> Result<String, String> {
    let parent = epub_path.parent().unwrap_or_else(|| Path::new("."));
    let mut cmd = Command::new("fanficfare");
    cmd.arg("-u")
        .arg("--non-interactive")
        .arg(epub_path)
        .current_dir(parent);
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run fanficfare: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        return Err(format!(
            "update failed: {}\n{}",
            stdout.trim(),
            stderr.trim()
        ));
    }
    Ok(format!("{}\n{}", stdout.trim(), stderr.trim()))
}

fn list_epubs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "epub") {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn newest_epubs(dir: &Path, n: usize) -> Vec<PathBuf> {
    let mut v = list_epubs(dir);
    v.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map(|t| {
                std::time::SystemTime::now()
                    .duration_since(t)
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    });
    v.into_iter().take(n).collect()
}
