//! Replacing a running executable with a downloaded update.
//!
//! The launcher cannot overwrite its own binary while it runs, so it writes a
//! tiny script that waits for this process to exit, swaps the file, starts the
//! new build and deletes itself. Nothing is installed without the user asking
//! for it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What the shell should do with a downloaded file.
pub enum Plan {
    /// Replace the running portable executable.
    Replace(PathBuf),
    /// Hand the package to the platform's installer.
    #[cfg_attr(not(windows), allow(dead_code))]
    Installer,
    /// Deb belongs to the system package manager.
    Manual,
}

pub fn plan(path: &Path) -> Plan {
    #[cfg(windows)]
    {
        if path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase().ends_with("setup.exe"))
            .unwrap_or(false)
        {
            return Plan::Installer;
        }
        return Plan::Replace(std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tntl.exe")));
    }
    #[cfg(target_os = "linux")]
    {
        if path
            .file_name()
            .map(|name| name.to_string_lossy().ends_with(".AppImage"))
            .unwrap_or(false)
        {
            if let Some(appimage) = std::env::var_os("APPIMAGE") {
                return Plan::Replace(PathBuf::from(appimage));
            }
        }
        return Plan::Manual;
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        Plan::Manual
    }
}

/// Start the swap and ask the app to exit. Returns the script path so the UI can
/// explain where it went.
pub fn apply(app: &tauri::AppHandle, file: &Path) -> Result<String, String> {
    match plan(file) {
        Plan::Manual => Err(file.display().to_string()),
        Plan::Installer => {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                Command::new(file)
                    .arg("/S")
                    .creation_flags(0x0800_0000)
                    .spawn()
                    .map_err(|error| error.to_string())?;
                app.exit(0);
                Ok("installer".to_string())
            }
            #[cfg(not(windows))]
            {
                Err(file.display().to_string())
            }
        }
        Plan::Replace(target) => {
            let script = spawn_swap(file, &target)?;
            app.exit(0);
            Ok(format!("replaced:{}", script.display()))
        }
    }
}

#[cfg(windows)]
fn spawn_swap(file: &Path, target: &Path) -> Result<PathBuf, String> {
    use std::os::windows::process::CommandExt;
    let dir = file
        .parent()
        .ok_or_else(|| "update file has no directory".to_string())?;
    let script = dir.join("apply-update.cmd");
    let pid = std::process::id();
    let body = format!(
        "@echo off\r\n\
         setlocal\r\n\
         :wait\r\n\
         tasklist /FI \"PID eq {pid}\" 2>nul | find \"{pid}\" >nul\r\n\
         if not errorlevel 1 (\r\n\
         \x20 timeout /t 1 /nobreak >nul\r\n\
         \x20 goto wait\r\n\
         )\r\n\
         copy /Y \"{new}\" \"{target}\" >nul\r\n\
         start \"\" \"{target}\"\r\n\
         del \"%~f0\"\r\n",
        new = file.display(),
        target = target.display()
    );
    std::fs::write(&script, body).map_err(|error| error.to_string())?;
    Command::new("cmd")
        .arg("/C")
        .arg("start")
        .arg("")
        .arg(&script)
        .creation_flags(0x0800_0000)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(script)
}

#[cfg(not(windows))]
fn spawn_swap(file: &Path, target: &Path) -> Result<PathBuf, String> {
    let dir = file
        .parent()
        .ok_or_else(|| "update file has no directory".to_string())?;
    let script = dir.join("apply-update.sh");
    let pid = std::process::id();
    let body = format!(
        "#!/bin/sh\n\
         while kill -0 {pid} 2>/dev/null; do sleep 1; done\n\
         mv -f '{new}' '{target}'\n\
         chmod +x '{target}'\n\
         '{target}' >/dev/null 2>&1 &\n\
         rm -f \"$0\"\n",
        new = file.display(),
        target = target.display()
    );
    std::fs::write(&script, &body).map_err(|error| error.to_string())?;
    Command::new("sh")
        .arg(&script)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(script)
}
