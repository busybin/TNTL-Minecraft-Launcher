//! Bisect executor: applies one test set, launches the game, watches for the
//! main menu, and asks `tntl-bisect` for the next set.
//!
//! The planner is pure and tested without a game. This module is deliberately
//! the only place that launches repeatedly: it restores the user's original
//! mod switches even when a launch, a dependency or the user interrupts.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tntl_bisect::{Assembly, Report, Session};
use tntl_instance::mods::ModInfo;
use tntl_launch::{LaunchOptions, reached_menu, read_tail};

use crate::{Launcher, VerifyMode};

const MENU_SETTLE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(250);
const STATE_FILE: &str = "bisect-state.json";

/// Written before the first test so a launcher crash cannot leave the mods in
/// a half-tested state. The next start restores it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SavedBisect {
    original: BTreeMap<String, bool>,
}

/// Puts the original switches back if the bisect unwinds before its normal
/// restore step (panic in the launch path). A hard kill or power loss leaves
/// `bisect-state.json` behind and the next start replays it.
struct RestoreOnDrop<'a> {
    launcher: &'a Launcher,
    name: &'a str,
    original: &'a BTreeMap<String, bool>,
    state_path: std::path::PathBuf,
    armed: bool,
}

impl Drop for RestoreOnDrop<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self.launcher.set_mods_enabled(self.name, self.original);
        let _ = std::fs::remove_file(&self.state_path);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BisectModView {
    pub file: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub proven: bool,
    pub modified: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BisectPlanView {
    pub total: usize,
    pub proven: usize,
    pub unproven: usize,
    pub suspects: Vec<BisectModView>,
    pub blocked: Vec<BisectModView>,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BisectTestView {
    pub index: usize,
    pub kind: String,
    pub enabled: usize,
    pub disabled: usize,
    pub crashed: bool,
    pub duration_ms: u64,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BisectReport {
    /// `culprit`, `interaction`, `not_reproduced`, `inconclusive` or `cancelled`.
    pub outcome: String,
    pub culprit: Option<BisectModView>,
    pub suspects: Vec<BisectModView>,
    pub also_disabled: Vec<BisectModView>,
    pub tests: Vec<BisectTestView>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BisectEvent {
    Plan {
        plan: BisectPlanView,
    },
    Test {
        index: usize,
        kind: String,
        enabled: usize,
        disabled: usize,
        total: usize,
    },
    Result {
        index: usize,
        crashed: bool,
        duration_ms: u64,
        summary: String,
    },
    Restoring,
    Finished {
        report: BisectReport,
    },
}

fn stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

fn view(module: &ModInfo) -> BisectModView {
    BisectModView {
        file: module.file.trim_end_matches(".disabled").to_string(),
        id: module.id.clone(),
        name: module.name.clone(),
        version: module.version.clone(),
        proven: module.proven,
        modified: module.modified,
    }
}

fn planner_mod(module: &ModInfo) -> tntl_bisect::Mod {
    tntl_bisect::Mod {
        key: module.file.trim_end_matches(".disabled").to_string(),
        id: module.id.clone(),
        version: module.version.clone(),
        enabled: module.enabled,
        proven: module.proven,
        blocked: module.blocked.is_some(),
        modified: module.modified,
        dependencies: module.dependency.clone(),
        provides: module.provides.clone(),
        nested: module.nested.clone(),
    }
}

impl Launcher {
    /// Restore switches left behind by a bisect that did not finish.
    pub fn recover_bisect_state(&self, name: &str) -> Result<()> {
        let dir = self.instance_dir(name)?;
        let path = dir.join(STATE_FILE);
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(());
        };
        let Ok(state): Result<SavedBisect, _> = serde_json::from_slice(&bytes) else {
            // A corrupt note is not worth blocking the instance forever.
            let _ = std::fs::remove_file(&path);
            return Ok(());
        };
        self.set_mods_enabled(name, &state.original)
            .with_context(|| format!("cannot restore the switches in {}", path.display()))?;
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// What a bisect would do, without touching a single jar. The UI shows this
    /// before the user commits to a series of launches.
    pub fn bisect_plan(&self, name: &str) -> Result<BisectPlanView> {
        let mods = self.mods(name)?;
        let assembly = Assembly::new(mods.iter().map(planner_mod).collect());
        let plan = assembly.plan();
        let by_key: BTreeMap<String, &ModInfo> = mods
            .iter()
            .map(|module| (module.file.trim_end_matches(".disabled").to_string(), module))
            .collect();
        let pick = |keys: &[String]| -> Vec<BisectModView> {
            keys.iter()
                .filter_map(|key| by_key.get(key).map(|module| view(module)))
                .collect()
        };
        Ok(BisectPlanView {
            total: plan.total,
            proven: plan.proven,
            unproven: plan.unproven,
            suspects: pick(&plan.suspects),
            blocked: pick(&plan.blocked),
            missing: plan.missing,
        })
    }

    /// Run a series of launches until a culprit (or an interaction) is found.
    ///
    /// `cancel` is polled between tests and while a game starts. The original
    /// enabled/disabled state is always restored before returning.
    pub fn bisect_mods(
        &self,
        name: &str,
        timeout: Duration,
        cancel: &AtomicBool,
        progress: &(dyn Fn(BisectEvent) + Send + Sync),
    ) -> Result<BisectReport> {
        self.ensure_not_running(name)?;
        self.recover_bisect_state(name)?;
        let instance = self.load_instance(name)?;
        if instance.max_memory_mb.is_none() {
            bail!("set the maximum memory for this instance before bisecting");
        }
        if !(Duration::from_secs(30)..=Duration::from_secs(900)).contains(&timeout) {
            bail!("the bisect timeout must be between 30 and 900 seconds");
        }

        let mods = self.mods(name)?;
        if mods.iter().any(|module| module.blocked.is_some() && module.enabled) {
            let names: Vec<&str> = mods
                .iter()
                .filter(|module| module.blocked.is_some() && module.enabled)
                .map(|module| module.file.as_str())
                .collect();
            bail!(
                "disable these jars the current loader cannot run first: {}",
                names.join(", ")
            );
        }
        let plan = self.bisect_plan(name)?;
        if !plan.missing.is_empty() {
            bail!("install the missing dependencies first: {}", plan.missing.join("; "));
        }
        if plan.total < 2 {
            bail!("this instance needs at least two enabled mods to bisect");
        }

        self.prepare_skin(name)?;
        let launch_plan = self.plan_instance(&instance, None, VerifyMode::Exists)?;
        let blocking: Vec<_> = launch_plan.blocking_missing().collect();
        if !blocking.is_empty() {
            bail!(
                "the instance is incomplete ({} files); run repair first",
                blocking.len()
            );
        }

        let assembly = Assembly::new(mods.iter().map(planner_mod).collect());
        let original: BTreeMap<String, bool> = mods
            .iter()
            .map(|module| (module.file.trim_end_matches(".disabled").to_string(), module.enabled))
            .collect();
        progress(BisectEvent::Plan { plan });

        let instance_dir = self.instance_dir(name)?;
        // Persist the original switches before the first rename. If this
        // process dies mid-test, the next start calls recover_bisect_state.
        let state_path = instance_dir.join(STATE_FILE);
        tntl_store::atomic_write_json(
            &state_path,
            &SavedBisect {
                original: original.clone(),
            },
        )?;
        let mut restore_on_drop = RestoreOnDrop {
            launcher: self,
            name,
            original: &original,
            state_path: state_path.clone(),
            armed: true,
        };

        let mut session = Session::new(assembly);
        let mut test = session.start();
        let mut tests: Vec<BisectTestView> = Vec::new();
        let mut cancelled = false;

        let result = 'tests: loop {
            if cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break Err(anyhow::anyhow!("bisect cancelled"));
            }
            let enabled: Vec<String> = test
                .resolution
                .enabled
                .iter()
                .map(|&index| mods[index].file.trim_end_matches(".disabled").to_string())
                .collect();
            let desired: BTreeMap<String, bool> = original
                .keys()
                .map(|key| (key.clone(), enabled.contains(key)))
                .collect();
            progress(BisectEvent::Test {
                index: test.index,
                kind: test.kind.to_string(),
                enabled: enabled.len(),
                disabled: original.len().saturating_sub(enabled.len()),
                total: original.len(),
            });
            if let Err(error) = self.set_mods_enabled(name, &desired) {
                break 'tests Err(error);
            }

            let log_path = instance_dir
                .join("logs")
                .join(format!("bisect-{}-{}.log", test.index, stamp()));
            let mut options = LaunchOptions::new(&log_path);
            // No pipe reader lives here: the game writes the log itself, and
            // this thread only tails it for the menu marker.
            options.detach_output = true;
            options.echo = false;
            options.clean_natives = false;

            if let Err(error) = self.claim_running(name, std::process::id()) {
                break 'tests Err(error);
            }
            let mut process = match tntl_launch::launch(&launch_plan, &options) {
                Ok(process) => process,
                Err(error) => {
                    self.clear_running(name);
                    break 'tests Err(error.into());
                }
            };
            if let Err(error) = self.mark_running(name, process.id()) {
                process.kill();
                self.clear_running(name);
                break 'tests Err(error);
            }

            let started = Instant::now();
            let deadline = started + timeout;
            let mut menu_at: Option<Instant> = None;
            let mut exit_status = None;
            let mut timed_out = false;
            loop {
                if cancel.load(Ordering::SeqCst) {
                    process.kill();
                    cancelled = true;
                }
                match process.try_wait() {
                    Ok(Some(status)) => {
                        exit_status = Some(status);
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        process.kill();
                        self.clear_running(name);
                        break 'tests Err(error.into());
                    }
                }
                if menu_at.is_none() {
                    let tail = read_tail(&log_path, 64 * 1024);
                    if reached_menu(&tail) {
                        menu_at = Some(Instant::now());
                    }
                }
                if menu_at.is_some_and(|at| at.elapsed() >= MENU_SETTLE) {
                    process.kill();
                    // Reap the process so the next test can claim the marker.
                    for _ in 0..40 {
                        match process.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                            Err(_) => break,
                        }
                    }
                    break;
                }
                if Instant::now() >= deadline {
                    timed_out = true;
                    process.kill();
                    for _ in 0..40 {
                        match process.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                            Err(_) => break,
                        }
                    }
                    break;
                }
                std::thread::sleep(POLL);
            }
            self.clear_running(name);
            let duration_ms = started.elapsed().as_millis() as u64;
            let log = read_tail(&log_path, 256 * 1024);

            if cancelled {
                break 'tests Err(anyhow::anyhow!("bisect cancelled"));
            }
            if timed_out && menu_at.is_none() {
                break 'tests Err(anyhow::anyhow!(
                    "the game did not reach the main menu in {}s; raise the timeout or check {}",
                    timeout.as_secs(),
                    log_path.display()
                ));
            }

            let crashed = if menu_at.is_some() {
                false
            } else {
                let exit_code = exit_status.and_then(|status| status.code());
                if exit_code == Some(0) && tntl_launch::diagnose(exit_code, &log).is_none() {
                    // Exited cleanly before the menu: not a crash, but not a
                    // successful start either. Guessing here would corrupt the
                    // bisect, so stop and show the log.
                    break 'tests Err(anyhow::anyhow!(
                        "the game exited before the main menu; see {}",
                        log_path.display()
                    ));
                }
                true
            };
            let exit_code = exit_status.and_then(|status| status.code());
            let summary = if menu_at.is_some() {
                "reached the main menu".to_string()
            } else {
                tntl_launch::diagnose(exit_code, &log)
                    .map(|crash| format!("{:?}: {}", crash.kind, crash.summary))
                    .unwrap_or_else(|| "exited".to_string())
            };
            progress(BisectEvent::Result {
                index: test.index,
                crashed,
                duration_ms,
                summary: summary.clone(),
            });
            tests.push(BisectTestView {
                index: test.index,
                kind: test.kind.to_string(),
                enabled: enabled.len(),
                disabled: original.len().saturating_sub(enabled.len()),
                crashed,
                duration_ms,
                summary,
            });

            match session.record(test, crashed) {
                Ok(Some(next)) => test = next,
                Ok(None) => break 'tests Ok(session.report().cloned()),
                Err(error) => break 'tests Err(error.into()),
            }
        };

        progress(BisectEvent::Restoring);
        restore_on_drop.armed = false;
        let restore = self.set_mods_enabled(name, &original);
        if restore.is_ok() {
            let _ = std::fs::remove_file(&state_path);
        }
        if let Err(error) = restore {
            // The report is still useful, but the user must know the switches
            // may be half applied.
            let mut report = self.report_view(&result, &session, &mods, &tests, cancelled);
            report.message = format!(
                "{} Restoring the original mod switches failed: {error:#}",
                report.message
            );
            progress(BisectEvent::Finished { report: report.clone() });
            return Ok(report);
        }

        let report = self.report_view(&result, &session, &mods, &tests, cancelled);
        let _ = tntl_store::atomic_write_json(
            &self.instance_dir(name)?.join("bisect-last.json"),
            &serde_json::json!({
                "at": stamp(),
                "outcome": report.outcome,
                "culprit": report.culprit.as_ref().map(|module| module.file.clone()),
                "tests": report.tests.len(),
            }),
        );
        progress(BisectEvent::Finished { report: report.clone() });
        Ok(report)
    }

    fn report_view(
        &self,
        result: &Result<Option<Report>>,
        session: &Session,
        mods: &[ModInfo],
        tests: &[BisectTestView],
        cancelled: bool,
    ) -> BisectReport {
        let by_index = |index: usize| -> BisectModView { view(&mods[index]) };
        let empty = BisectReport {
            outcome: "inconclusive".to_string(),
            culprit: None,
            suspects: Vec::new(),
            also_disabled: Vec::new(),
            tests: tests.to_vec(),
            message: String::new(),
        };
        if cancelled {
            return BisectReport {
                outcome: "cancelled".to_string(),
                message: "The bisect was stopped; the original mod switches are back.".to_string(),
                ..empty
            };
        }
        match result {
            Err(error) => BisectReport {
                outcome: "inconclusive".to_string(),
                message: format!("{error:#}"),
                ..empty
            },
            Ok(Some(Report::Culprit {
                module,
                also_disabled,
                tests: count,
            })) => BisectReport {
                outcome: "culprit".to_string(),
                culprit: Some(by_index(*module)),
                also_disabled: also_disabled.iter().map(|&index| by_index(index)).collect(),
                message: format!("Found it in {count} launches."),
                ..empty
            },
            Ok(Some(Report::Interaction { suspects, tests: count })) => BisectReport {
                outcome: "interaction".to_string(),
                suspects: suspects.iter().map(|&index| by_index(index)).collect(),
                message: format!(
                    "No single jar is responsible; {count} launches narrowed it to {} suspects.",
                    suspects.len()
                ),
                ..empty
            },
            Ok(Some(Report::NotReproduced)) => BisectReport {
                outcome: "not_reproduced".to_string(),
                message: "The full mod set did not crash; nothing to bisect.".to_string(),
                ..empty
            },
            Ok(Some(Report::Inconclusive { reason, tests: count })) => BisectReport {
                outcome: "inconclusive".to_string(),
                message: format!("{reason} after {count} launches."),
                ..empty
            },
            Ok(None) => BisectReport {
                outcome: "inconclusive".to_string(),
                message: format!(
                    "The planner stopped without a report after {} launches.",
                    session.tests()
                ),
                ..empty
            },
        }
    }
}
