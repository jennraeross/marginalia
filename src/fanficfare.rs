use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

/// Update-check state for one tracked book (session only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    UpToDate,
    UpdateAvailable { new_chapters: usize },
}

/// Compare a local chapter count against remote metadata: more chapters
/// remotely means an update is ready. `None` (no remote count) stays unknown.
pub fn compare_chapters(local: usize, remote: Option<usize>) -> Option<CheckState> {
    let r = remote?;
    if r > local {
        Some(CheckState::UpdateAvailable {
            new_chapters: r - local,
        })
    } else {
        Some(CheckState::UpToDate)
    }
}

/// Remote story metadata from a metadata-only fetch (no chapters downloaded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMeta {
    pub chapters: Option<usize>,
    pub status: String,
    pub updated: String,
}

/// Parse `fanficfare --meta-only --json-meta` output: a flat string map with
/// keys like `numChapters` ("12", "1,234"), `status`, `dateUpdated`.
/// Unknown shapes parse to unknown fields rather than failing outright.
pub fn parse_remote_meta(json: &str) -> Result<RemoteMeta, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("metadata is not JSON: {e}"))?;
    let s = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_default()
    };
    // `numChapters` is a string that may carry commas ("1,234"); accept a
    // bare JSON number too, and fall back to a `chapters` array length.
    let chapters = v
        .get("numChapters")
        .and_then(|x| {
            x.as_str()
                .map(|s| s.chars().filter(|c| c.is_ascii_digit()).collect())
                .or_else(|| x.as_u64().map(|n| n.to_string()))
        })
        .filter(|s: &String| !s.is_empty())
        .and_then(|s| s.parse::<usize>().ok())
        .or_else(|| {
            v.get("chapters")
                .and_then(|x| x.as_array())
                .map(|a| a.len())
        });
    Ok(RemoteMeta {
        chapters,
        status: s("status"),
        updated: s("dateUpdated"),
    })
}

/// Fetch story metadata without downloading chapters. Runs in a temp dir so
/// no titlepage file can land in the working directory, and kills a hung
/// fetch after `timeout`.
pub fn fetch_meta(url: &str, timeout: Duration) -> Result<RemoteMeta, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("tempdir failed: {e}"))?;
    let mut child = Command::new("fanficfare")
        .arg("--meta-only")
        .arg("--json-meta")
        .arg("--no-output")
        .arg(url)
        .current_dir(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run fanficfare: {e}"))?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait().map_err(|e| format!("wait failed: {e}"))? {
            Some(status) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    use std::io::Read;
                    let _ = so.read_to_string(&mut out);
                }
                if !status.success() {
                    let mut err = String::new();
                    if let Some(mut se) = child.stderr.take() {
                        use std::io::Read;
                        let _ = se.read_to_string(&mut err);
                    }
                    return Err(format!("metadata fetch failed: {}", err.trim()));
                }
                return parse_remote_meta(&out);
            }
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("metadata fetch timed out after {timeout:?}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_meta_chapters_and_status() {
        let m = parse_remote_meta(
            r#"{"dateUpdated":"2026-11-01","numChapters":"12","status":"In-Progress","title":"T"}"#,
        )
        .unwrap();
        assert_eq!(m.chapters, Some(12));
        assert_eq!(m.status, "In-Progress");
        assert_eq!(m.updated, "2026-11-01");
    }

    #[test]
    fn parse_meta_tolerates_shapes() {
        // Comma-grouped counts.
        let m = parse_remote_meta(r#"{"numChapters":"1,234"}"#).unwrap();
        assert_eq!(m.chapters, Some(1234));
        // Bare JSON number.
        let m = parse_remote_meta(r#"{"numChapters":7}"#).unwrap();
        assert_eq!(m.chapters, Some(7));
        // Missing count stays unknown, other fields default empty.
        let m = parse_remote_meta(r#"{"title":"T"}"#).unwrap();
        assert_eq!(m.chapters, None);
        assert!(m.status.is_empty());
        // Not JSON at all is an error.
        assert!(parse_remote_meta("not json").is_err());
    }

    #[test]
    fn compare_chapters_marks_growth() {
        assert_eq!(
            compare_chapters(10, Some(12)),
            Some(CheckState::UpdateAvailable { new_chapters: 2 })
        );
        assert_eq!(compare_chapters(12, Some(12)), Some(CheckState::UpToDate));
        assert_eq!(compare_chapters(13, Some(12)), Some(CheckState::UpToDate));
        assert_eq!(compare_chapters(10, None), None);
    }
}
