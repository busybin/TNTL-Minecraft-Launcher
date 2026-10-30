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
                // `$APPIMAGE` may be a symlink; replace the real file so the
                // desktop entry keeps pointing at an executable.
                let path = PathBuf::from(appimage);
                return Plan::Replace(std::fs::canonicalize(&path).unwrap_or(path));
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
        // Not something we may install: the caller opens the folder instead.
        Plan::Manual => Ok(format!("manual:{}", file.display())),
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
                Ok(format!("manual:{}", file.display()))
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
    ensure_writable(target)?;
    let script = dir.join("apply-update.cmd");
    let pid = std::process::id();
    // `%` is expanded by cmd even inside quotes.
    let new = file.display().to_string().replace('%', "%%");
    let target_text = target.display().to_string().replace('%', "%%");
    let body = format!(
        "@echo off\r\n\
         setlocal\r\n\
         rem Absolute paths only: cmd searches the working directory first.\r\n\
         set \"PATH=%SystemRoot%\\System32\"\r\n\
         :wait\r\n\
         \"%SystemRoot%\\System32\\tasklist.exe\" /FI \"PID eq {pid}\" /NH /FO CSV 2>nul | \"%SystemRoot%\\System32\\findstr.exe\" /C:\"\\\"{pid}\\\"\" >nul\r\n\
         if not errorlevel 1 (\r\n\
         \x20 \"%SystemRoot%\\System32\\timeout.exe\" /t 1 /nobreak >nul\r\n\
         \x20 goto wait\r\n\
         )\r\n\
         copy /Y \"{target_text}\" \"{target_text}.old\" >nul 2>&1\r\n\
         copy /Y \"{new}\" \"{target_text}\" >nul 2>&1\r\n\
         if errorlevel 1 (\r\n\
         \x20 copy /Y \"{target_text}.old\" \"{target_text}\" >nul 2>&1\r\n\
         \x20 echo update failed, the previous version was restored > \"%~dp0apply-update.log\"\r\n\
         \x20 del \"%~f0\"\r\n\
         \x20 exit /b 1\r\n\
         )\r\n\
         del \"{target_text}.old\" >nul 2>&1\r\n\
         start \"\" \"{target_text}\"\r\n\
         del \"%~f0\"\r\n"
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
    ensure_writable(target)?;
    let script = dir.join("apply-update.sh");
    let pid = std::process::id();
    let quote = |value: &Path| format!("'{}'", value.display().to_string().replace('\'', "'\\''"));
    let body = format!(
        "#!/bin/sh\n\
         # Absolute tools only: PATH is inherited from the launcher.\n\
         while kill -0 {pid} 2>/dev/null; do sleep 1; done\n\
         /bin/cp -f {target} {target}.old 2>/dev/null\n\
         if /bin/mv -f {new} {target}; then\n\
         \x20 /bin/chmod +x {target}\n\
         \x20 /bin/rm -f {target}.old\n\
         else\n\
         \x20 /bin/cp -f {target}.old {target} 2>/dev/null\n\
         \x20 echo \"update failed, the previous version was restored\" > \"$(dirname \"$0\")/apply-update.log\"\n\
         \x20 /bin/rm -f \"$0\"\n\
         \x20 exit 1\n\
         fi\n\
         /bin/rm -f \"$0\"\n\
         exec {target} >/dev/null 2>&1\n",
        new = quote(file),
        target = quote(target)
    );
    std::fs::write(&script, &body).map_err(|error| error.to_string())?;
    Command::new("/bin/sh")
        .arg(&script)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(script)
}

/// Refuse to schedule a swap the process cannot perform: exiting the app and
/// failing silently is worse than reporting the problem now.
fn ensure_writable(target: &Path) -> Result<(), String> {
    let dir = target
        .parent()
        .ok_or_else(|| "update target has no directory".to_string())?;
    let probe = dir.join(".tntl-update-probe");
    let writable = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    if !writable {
        return Err(format!("{} is not writable", dir.display()));
    }
    Ok(())
}
