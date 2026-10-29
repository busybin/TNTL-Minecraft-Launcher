//! Spawn the game, pump its output to a log file and classify failures.

#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use tntl_resolve::LaunchPlan;

/// Receives one line of game output as it is produced.
pub type LineSink = Arc<dyn Fn(&str) + Send + Sync>;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("natives extraction failed: {0}")]
    Natives(String),
    #[error("legacy asset tree failed: {0}")]
    Assets(String),
    #[error("could not spawn {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone)]
pub struct LaunchOptions {
    pub log_path: PathBuf,
    /// Mirror game output to this process' stdout/stderr.
    pub echo: bool,
    /// Delete the natives directory before extracting.
    pub clean_natives: bool,
    /// Receives every output line as it is written, for live consoles.
    pub line_sink: Option<LineSink>,
    /// Give the game its own handles to the log file instead of pumping the
    /// pipes here. Needed when the caller exits while the game keeps running.
    pub detach_output: bool,
}

impl std::fmt::Debug for LaunchOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LaunchOptions")
            .field("log_path", &self.log_path)
            .field("echo", &self.echo)
            .field("clean_natives", &self.clean_natives)
            .field("line_sink", &self.line_sink.is_some())
            .field("detach_output", &self.detach_output)
            .finish()
    }
}

impl LaunchOptions {
    pub fn new(log_path: impl Into<PathBuf>) -> Self {
        Self {
            log_path: log_path.into(),
            echo: false,
            clean_natives: true,
            line_sink: None,
            detach_output: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashKind {
    Terminated,
    OutOfMemory,
    NativeCrash,
    ModResolution,
    MissingMainClass,
    UnsupportedJava,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct CrashReport {
    pub kind: CrashKind,
    pub summary: String,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LaunchOutcome {
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub log_path: PathBuf,
    pub crash: Option<CrashReport>,
}

pub struct GameProcess {
    child: Arc<Mutex<Child>>,
    log_path: PathBuf,
    started: Instant,
    pumps: Vec<JoinHandle<()>>,
}

/// Cheap handle that can stop the game from another thread.
#[derive(Clone)]
pub struct GameHandle {
    child: Arc<Mutex<Child>>,
}

impl GameHandle {
    pub fn kill(&self) {
        let mut child = self.child.lock().unwrap_or_else(|poison| poison.into_inner());
        let _ = child.kill();
    }
}

impl GameProcess {
    pub fn id(&self) -> u32 {
        self.child.lock().unwrap_or_else(|poison| poison.into_inner()).id()
    }

    pub fn handle(&self) -> GameHandle {
        GameHandle {
            child: Arc::clone(&self.child),
        }
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .try_wait()
    }

    pub fn kill(&mut self) {
        self.handle().kill();
    }

    pub fn wait(self) -> Result<LaunchOutcome, LaunchError> {
        self.wait_timeout(None)
    }

    /// Wait for the game, killing it when `timeout` elapses. Used by smoke
    /// tests so a stuck client cannot hang CI.
    pub fn wait_timeout(mut self, timeout: Option<std::time::Duration>) -> Result<LaunchOutcome, LaunchError> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let status = loop {
            let exited = self
                .child
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .try_wait()
                .map_err(|source| LaunchError::Spawn {
                    path: self.log_path.clone(),
                    source,
                })?;
            if let Some(status) = exited {
                break status;
            }
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                let mut child = self.child.lock().unwrap_or_else(|poison| poison.into_inner());
                let _ = child.kill();
                break child.wait().map_err(|source| LaunchError::Spawn {
                    path: self.log_path.clone(),
                    source,
                })?;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        for pump in self.pumps.drain(..) {
            let _ = pump.join();
        }
        let log = read_tail(&self.log_path, 256 * 1024);
        Ok(LaunchOutcome {
            exit_code: status.code(),
            duration_ms: self.started.elapsed().as_millis(),
            log_path: self.log_path.clone(),
            crash: diagnose(status.code(), &log),
        })
    }
}

pub fn launch(plan: &LaunchPlan, options: &LaunchOptions) -> Result<GameProcess, LaunchError> {
    prepare_legacy_assets(plan)?;
    prepare_natives(plan, options.clean_natives)?;
    fs::create_dir_all(&plan.working_dir).map_err(|source| LaunchError::Io {
        path: plan.working_dir.clone(),
        source,
    })?;
    if let Some(parent) = options.log_path.parent() {
        fs::create_dir_all(parent).map_err(|source| LaunchError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&options.log_path)
        .map_err(|source| LaunchError::Io {
            path: options.log_path.clone(),
            source,
        })?;
    let mut command = Command::new(launch_binary(&plan.java));
    command
        .args(&plan.jvm_args)
        .arg(&plan.main_class)
        .args(&plan.game_args)
        .current_dir(&plan.working_dir)
        .envs(&plan.env)
        .stdin(Stdio::null());
    // `java.exe` is a console program; spawned from the GUI shell it would open
    // a window that stays for the whole session (and closing it kills the game).
    // `javaw.exe` is the window-less twin, and the flag covers a custom runtime
    // where only `java.exe` exists.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(unix)]
    apply_utf8_locale(&mut command, &plan.env);
    if options.detach_output {
        let out = file.try_clone().map_err(|source| LaunchError::Io {
            path: options.log_path.clone(),
            source,
        })?;
        let err = file.try_clone().map_err(|source| LaunchError::Io {
            path: options.log_path.clone(),
            source,
        })?;
        command.stdout(Stdio::from(out)).stderr(Stdio::from(err));
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let log = Arc::new(Mutex::new(file));

    let mut child = command.spawn().map_err(|source| LaunchError::Spawn {
        path: plan.java.clone(),
        source,
    })?;

    let mut pumps = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        pumps.push(pump(
            stdout,
            log.clone(),
            options.echo,
            false,
            options.line_sink.clone(),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        pumps.push(pump(stderr, log.clone(), options.echo, true, options.line_sink.clone()));
    }

    Ok(GameProcess {
        child: Arc::new(Mutex::new(child)),
        log_path: options.log_path.clone(),
        started: Instant::now(),
        pumps,
    })
}

/// The JVM binary to spawn. The JDK ships `javaw.exe` next to `java.exe`; it is
/// the same JVM as a GUI-subsystem process, so no console window appears. When
/// only `java.exe` exists the hidden-console flag covers it.
fn launch_binary(java: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if java
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("java.exe"))
        {
            let javaw = java.with_file_name("javaw.exe");
            if javaw.is_file() {
                return javaw;
            }
        }
    }
    java.to_path_buf()
}

/// Java decodes file names with the process locale. Under `LANG=POSIX` or no
/// locale at all (ssh, cron) it falls back to ASCII and refuses packs such as
/// `LowOnFire v26.2§8.zip` with `InvalidPathException`. Fill in a UTF-8 locale
/// only when the caller did not pick one.
#[cfg(unix)]
fn apply_utf8_locale(command: &mut Command, env: &std::collections::BTreeMap<String, String>) {
    if env.contains_key("LANG") || env.contains_key("LC_ALL") {
        return;
    }
    let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|key| {
        std::env::var(key).is_ok_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        })
    });
    if !utf8 {
        command.env("LANG", "C.UTF-8");
    }
}

fn pump<R: Read + Send + 'static>(
    reader: R,
    log: Arc<Mutex<File>>,
    echo: bool,
    stderr: bool,
    sink: Option<LineSink>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            match reader.read_until(b'\n', &mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
            while matches!(buffer.last(), Some(b'\n' | b'\r')) {
                buffer.pop();
            }
            // The JVM on Windows writes the console code page, not UTF-8; a
            // single invalid line must not stop the log pump.
            let line = String::from_utf8_lossy(&buffer);
            {
                let mut file = log.lock().unwrap_or_else(|poison| poison.into_inner());
                let _ = writeln!(file, "{line}");
            }
            if let Some(sink) = &sink {
                sink(&line);
            }
            if echo {
                if stderr {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                }
            }
        }
    })
}

/// Pre-1.7 versions read a plain tree of asset names instead of the object
/// store. Installers build the shared one, but `mapToResources` indexes need it
/// inside the instance's game directory, which only the launch knows.
fn prepare_legacy_assets(plan: &LaunchPlan) -> Result<(), LaunchError> {
    let Some(tree) = &plan.legacy_assets else {
        return Ok(());
    };
    tntl_store::materialize_asset_index(&tntl_store::RootLayout::new(&tree.root), &tree.index, &tree.dir, false)
        .map_err(|error| LaunchError::Assets(error.to_string()))?;
    Ok(())
}

fn prepare_natives(plan: &LaunchPlan, clean: bool) -> Result<(), LaunchError> {
    if clean {
        match fs::remove_dir_all(&plan.natives_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(LaunchError::Io {
                    path: plan.natives_dir.clone(),
                    source,
                });
            }
        }
    }
    fs::create_dir_all(&plan.natives_dir).map_err(|source| LaunchError::Io {
        path: plan.natives_dir.clone(),
        source,
    })?;
    if plan.natives.is_empty() {
        return Ok(());
    }
    // Extract in reverse order: earlier libraries win when two jars provide the
    // same file, which matches how the vanilla launcher stacks natives.
    for native in plan.natives.iter().rev() {
        tntl_store::extract_natives(&native.path, &plan.natives_dir, &native.excludes)
            .map_err(|error| LaunchError::Natives(error.to_string()))?;
    }
    Ok(())
}

/// Read at most `max_bytes` from the end of a file as UTF-8 (lossy).
pub fn read_tail(path: &Path, max_bytes: u64) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buffer = Vec::with_capacity((len - start) as usize);
    if file.read_to_end(&mut buffer).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

pub fn diagnose(exit_code: Option<i32>, log: &str) -> Option<CrashReport> {
    if exit_code == Some(0) {
        return None;
    }
    if exit_code.is_none() || matches!(exit_code, Some(130) | Some(143)) {
        return Some(CrashReport {
            kind: CrashKind::Terminated,
            summary: "The game was terminated by a signal.".to_string(),
            hints: Vec::new(),
        });
    }
    let report = |kind, summary: &str, hints: &[&str]| CrashReport {
        kind,
        summary: summary.to_string(),
        hints: hints.iter().map(|hint| hint.to_string()).collect(),
    };
    if log.contains("java.lang.OutOfMemoryError") {
        return Some(report(
            CrashKind::OutOfMemory,
            "The game ran out of memory.",
            &[
                "Increase the maximum memory of this instance.",
                "Remove or disable memory-heavy mods and resource packs.",
            ],
        ));
    }
    if log.contains("UnsupportedClassVersionError") {
        return Some(report(
            CrashKind::UnsupportedJava,
            "The selected Java is too old for this version.",
            &["Pick a newer Java runtime for this instance."],
        ));
    }
    if log.contains("Could not find or load main class") || log.contains("Error: Could not find or load main class") {
        return Some(report(
            CrashKind::MissingMainClass,
            "The main class is missing from the classpath.",
            &["Run `tntl verify` to find missing libraries or the client jar."],
        ));
    }
    if log.contains("A fatal error has been detected by the Java Runtime Environment")
        || log.contains("EXCEPTION_ACCESS_VIOLATION")
        || log.contains("SIGSEGV")
    {
        return Some(report(
            CrashKind::NativeCrash,
            "The JVM crashed in native code (graphics driver or natives).",
            &[
                "Update the graphics driver.",
                "Try a different Java runtime.",
                "Check the hs_err file next to the game directory.",
            ],
        ));
    }
    if log.contains("Incompatible mod set")
        || log.contains("Missing or unsupported mandatory dependencies")
        || log.contains("Mod resolution failed")
    {
        return Some(report(
            CrashKind::ModResolution,
            "Mods could not be resolved.",
            &["Open the mod list and read the missing dependency message."],
        ));
    }
    let last = log
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no output")
        .to_string();
    Some(report(
        CrashKind::Unknown,
        &format!("Game exited with {exit_code:?}: {last}"),
        &[],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_assets_are_materialized_into_the_game_directory() {
        let dir = std::env::temp_dir().join(format!("tntl-launch-legacy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let roots = tntl_store::RootLayout::new(&dir);
        let object = roots.asset_object("aa11");
        fs::create_dir_all(object.parent().unwrap()).unwrap();
        fs::write(&object, b"sound").unwrap();
        let index = roots.asset_index("pre-1.6");
        fs::create_dir_all(index.parent().unwrap()).unwrap();
        fs::write(
            &index,
            r#"{"map_to_resources":true,"objects":{"sound/one.ogg":{"hash":"aa11","size":5}}}"#,
        )
        .unwrap();
        let game_dir = dir.join("instances/one/minecraft");
        let plan = LaunchPlan {
            version_id: "1.5.2".to_string(),
            java: PathBuf::from("/bin/true"),
            jvm_args: Vec::new(),
            main_class: "net.minecraft.client.main.Main".to_string(),
            classpath: Vec::new(),
            natives_dir: dir.join("natives"),
            natives: Vec::new(),
            game_args: Vec::new(),
            working_dir: game_dir.clone(),
            env: Default::default(),
            asset_index: Some(index.clone()),
            asset_dir: game_dir.join("resources"),
            legacy_assets: Some(tntl_resolve::LegacyAssetTree {
                index,
                dir: game_dir.join("resources"),
                root: dir.clone(),
            }),
            warnings: Vec::new(),
            missing: Vec::new(),
        };
        prepare_legacy_assets(&plan).unwrap();
        assert_eq!(fs::read(game_dir.join("resources/sound/one.ogg")).unwrap(), b"sound");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clean_exit_is_not_a_crash() {
        assert!(diagnose(Some(0), "anything").is_none());
    }

    #[test]
    fn classifies_common_failures() {
        let oom = diagnose(
            Some(1),
            "Exception in thread main java.lang.OutOfMemoryError: Java heap space",
        )
        .unwrap();
        assert_eq!(oom.kind, CrashKind::OutOfMemory);
        let java = diagnose(Some(1), "java.lang.UnsupportedClassVersionError: 65.0").unwrap();
        assert_eq!(java.kind, CrashKind::UnsupportedJava);
        let unknown = diagnose(Some(2), "boom").unwrap();
        assert_eq!(unknown.kind, CrashKind::Unknown);
    }

    #[test]
    fn tail_reads_the_end() {
        let path = std::env::temp_dir().join(format!("tntl-tail-{}", std::process::id()));
        fs::write(&path, "0123456789").unwrap();
        assert_eq!(read_tail(&path, 4), "6789");
        assert_eq!(read_tail(&path, 100), "0123456789");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn pump_keeps_going_after_invalid_utf8() {
        let path = std::env::temp_dir().join(format!("tntl-pump-{}", std::process::id()));
        let log = Arc::new(Mutex::new(File::create(&path).unwrap()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let collected = seen.clone();
        let sink: LineSink = Arc::new(move |line: &str| {
            collected.lock().unwrap().push(line.to_string());
        });
        let input = b"first\n\xff\xfe broken\nlast\n".to_vec();
        pump(std::io::Cursor::new(input), log, false, false, Some(sink))
            .join()
            .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("first"), "{text}");
        assert!(text.contains("last"), "{text}");
        let lines = seen.lock().unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2], "last");
        let _ = fs::remove_file(path);
    }
}
