//! JVM discovery and matching. Windows and Linux only.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JavaInstall {
    pub path: PathBuf,
    pub version: String,
    pub major: u32,
    pub vendor: Option<String>,
    pub arch: Option<String>,
}

impl JavaInstall {
    pub fn home(&self) -> Option<PathBuf> {
        self.path.parent()?.parent().map(Path::to_path_buf)
    }
}

/// Run a JVM and parse `-XshowSettings:properties -version`.
pub fn probe(path: &Path) -> Option<JavaInstall> {
    let output = Command::new(path)
        .args(["-XshowSettings:properties", "-version"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stderr);
    let mut properties = std::collections::BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(' ') {
            continue;
        }
        properties.insert(key.to_string(), value.trim().to_string());
    }
    let version = properties.get("java.version")?.clone();
    let major = properties
        .get("java.specification.version")
        .and_then(|value| parse_major(value))
        .or_else(|| parse_major(&version))?;
    Some(JavaInstall {
        path: path.to_path_buf(),
        version,
        major,
        vendor: properties.get("java.vendor").cloned(),
        arch: properties.get("os.arch").cloned(),
    })
}

fn parse_major(value: &str) -> Option<u32> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix("1.") {
        return rest.split('.').next()?.parse().ok();
    }
    value.split('.').next()?.parse().ok()
}

/// All JVMs we can find without asking the user.
pub fn discover() -> Vec<JavaInstall> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        candidates.push(PathBuf::from(home).join(bin_name()));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join(bin_name()));
        }
    }
    for dir in search_dirs() {
        collect_from_dir(&dir, &mut candidates, 0);
    }
    collect_home_jdks(&mut candidates);
    probe_all(candidates)
}

/// JVMs unpacked below `dir`; used for the launcher's own runtimes.
pub fn discover_in(dir: &Path) -> Vec<JavaInstall> {
    let mut candidates = Vec::new();
    collect_from_dir(dir, &mut candidates, 0);
    probe_all(candidates)
}

fn probe_all(candidates: Vec<PathBuf>) -> Vec<JavaInstall> {
    let mut seen = BTreeSet::new();
    let mut installs = Vec::new();
    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if let Some(install) = probe(&candidate) {
            installs.push(install);
        }
    }
    installs.sort_by_key(|install| (install.major, install.path.clone()));
    installs
}

/// Pick the JVM for a version that requires `required` major.
///
/// Exact match first, then the smallest newer major, then the newest JVM we
/// have (the caller decides whether to warn).
/// Exact match first, then the smallest installed JVM that is new enough. An
/// older JVM cannot load newer class files, so there is no fallback: callers
/// report the missing runtime instead.
pub fn select(installs: &[JavaInstall], required: u32) -> Option<&JavaInstall> {
    if let Some(exact) = installs.iter().find(|install| install.major == required) {
        return Some(exact);
    }
    installs
        .iter()
        .filter(|install| install.major > required)
        .min_by_key(|install| install.major)
}

fn bin_name() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(variable) {
                let base = PathBuf::from(base);
                dirs.push(base.join("Java"));
                dirs.push(base.join("Eclipse Adoptium"));
                dirs.push(base.join("Microsoft"));
                dirs.push(base.join("Programs").join("Eclipse Adoptium"));
                dirs.push(
                    base.join("Packages")
                        .join("Microsoft.4297127D64EC6_8wekyb3d8bbwe")
                        .join("LocalCache")
                        .join("Local")
                        .join("runtime"),
                );
            }
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            dirs.push(PathBuf::from(profile).join(".jdks"));
        }
    } else {
        dirs.push(PathBuf::from("/usr/lib/jvm"));
        dirs.push(PathBuf::from("/usr/java"));
        dirs.push(PathBuf::from("/opt/java"));
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            dirs.push(home.join(".jdks"));
            dirs.push(home.join(".local").join("share").join("jdks"));
        }
    }
    dirs
}

/// JDKs unpacked into the home directory, plus launcher-managed runtimes.
fn collect_home_jdks(out: &mut Vec<PathBuf>) {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return;
    };
    let home = PathBuf::from(home);
    if let Ok(entries) = std::fs::read_dir(&home) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !(name.contains("jdk") || name.contains("jre") || name.contains("graal") || name.starts_with("java")) {
                continue;
            }
            let java = entry.path().join("bin").join(bin_name());
            if java.is_file() {
                out.push(java);
            } else {
                collect_from_dir(&entry.path(), out, 0);
            }
        }
    }
    collect_from_dir(&home.join(".minecraft").join("runtime"), out, 0);
}

/// Look for `bin/java` at most three levels below `dir`.
fn collect_from_dir(dir: &Path, out: &mut Vec<PathBuf>, depth: u8) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let java = path.join("bin").join(bin_name());
        if java.is_file() {
            out.push(java);
        }
        collect_from_dir(&path, out, depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_java_versions() {
        assert_eq!(parse_major("17.0.2"), Some(17));
        assert_eq!(parse_major("1.8.0_402"), Some(8));
        assert_eq!(parse_major("21"), Some(21));
        assert_eq!(parse_major("nope"), None);
    }

    #[test]
    fn selects_exact_then_newer_but_never_older() {
        let make = |major| JavaInstall {
            path: PathBuf::from(format!("/jvm/{major}/bin/java")),
            version: format!("{major}.0.0"),
            major,
            vendor: None,
            arch: None,
        };
        let installs = vec![make(8), make(17), make(21)];
        assert_eq!(select(&installs, 17).unwrap().major, 17);
        assert_eq!(select(&installs, 16).unwrap().major, 17);
        assert_eq!(select(&installs, 8).unwrap().major, 8);
        assert!(select(&installs, 22).is_none());
    }
}
