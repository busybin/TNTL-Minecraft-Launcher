//! Dependency-aware mod bisect planning.
//!
//! The crate never launches a game and never touches the disk: it turns a mod
//! list plus a crash/no-crash observation into the next test set. That keeps
//! the decision logic deterministic and unit-testable; the executor in
//! `tntl-app` only toggles files and reports back.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mod {
    /// Stable file name without the `.disabled` suffix.
    pub key: String,
    pub id: String,
    pub version: String,
    pub enabled: bool,
    pub proven: bool,
    pub blocked: bool,
    pub modified: u64,
    pub dependencies: Vec<String>,
    pub provides: Vec<String>,
    pub nested: Vec<String>,
}

impl Mod {
    fn supplied_ids(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str())
            .chain(self.provides.iter().map(String::as_str))
            .chain(self.nested.iter().map(String::as_str))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// Mods that would be switched on for this test.
    pub enabled: BTreeSet<usize>,
    /// Enabled only because another enabled mod needs them.
    pub forced: BTreeSet<usize>,
    /// Enabled mods that had to be dropped because a dependency is unavailable.
    pub missing: BTreeMap<usize, Vec<String>>,
    /// Blocked jars that were part of the seed and had to be dropped.
    pub blocked: BTreeSet<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Assembly {
    pub mods: Vec<Mod>,
    providers: HashMap<String, Vec<usize>>,
}

impl Assembly {
    pub fn new(mods: Vec<Mod>) -> Self {
        let mut providers: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, module) in mods.iter().enumerate() {
            for id in module.supplied_ids() {
                let id = id.to_ascii_lowercase();
                let entry = providers.entry(id).or_default();
                if !entry.contains(&index) {
                    entry.push(index);
                }
            }
        }
        Self { mods, providers }
    }

    pub fn all_original_enabled(&self) -> BTreeSet<usize> {
        self.mods
            .iter()
            .enumerate()
            .filter(|(_, module)| module.enabled)
            .map(|(index, _)| index)
            .collect()
    }

    /// Enabled jars that are not proven yet, newest first.
    pub fn unproven(&self) -> Vec<usize> {
        let mut list: Vec<usize> = self
            .mods
            .iter()
            .enumerate()
            .filter(|(_, module)| module.enabled && !module.proven && !module.blocked)
            .map(|(index, _)| index)
            .collect();
        list.sort_by_key(|&index| std::cmp::Reverse(self.mods[index].modified));
        list
    }

    /// Describe the instance before a session starts.
    pub fn plan(&self) -> Plan {
        let resolution = self.resolve(&self.all_original_enabled());
        Plan {
            total: self.mods.iter().filter(|module| module.enabled).count(),
            proven: self
                .mods
                .iter()
                .filter(|module| module.enabled && module.proven)
                .count(),
            unproven: self.unproven().len(),
            blocked: self
                .mods
                .iter()
                .filter(|module| module.enabled && module.blocked)
                .map(|module| module.key.clone())
                .collect(),
            missing: resolution
                .missing
                .iter()
                .flat_map(|(index, deps)| {
                    deps.iter()
                        .map(move |dep| format!("{} needs {dep}", self.mods[*index].key))
                })
                .collect(),
            suspects: self
                .unproven()
                .into_iter()
                .map(|index| self.mods[index].key.clone())
                .collect(),
        }
    }

    /// Resolve a requested seed into a loadable set.
    ///
    /// Two rules run to a fixed point:
    /// 1. an enabled mod's dependencies are enabled too;
    /// 2. an enabled mod whose dependency has no enabled provider is dropped,
    ///    which recursively drops everything that depends on it.
    ///
    /// Rule 2 is the "stopping a mod stops its dependents" requirement: the
    /// caller never has to compute a reverse graph by hand.
    pub fn resolve(&self, seed: &BTreeSet<usize>) -> Resolution {
        let mut resolution = Resolution::default();
        for &index in seed {
            if self.mods.get(index).is_none() {
                continue;
            }
            if self.mods[index].blocked {
                resolution.blocked.insert(index);
            } else {
                resolution.enabled.insert(index);
            }
        }

        let limit = self.mods.len() * 4 + 16;
        for _ in 0..limit {
            let mut changed = false;

            let snapshot: Vec<usize> = resolution.enabled.iter().copied().collect();
            for index in snapshot {
                let dependencies = self.mods[index].dependencies.clone();
                for dependency in &dependencies {
                    if self.has_enabled_provider(dependency, &resolution.enabled) {
                        continue;
                    }
                    if let Some(provider) = self.pick_provider(dependency, &resolution.enabled) {
                        resolution.enabled.insert(provider);
                        resolution.forced.insert(provider);
                        changed = true;
                    }
                }
            }
            if changed {
                continue;
            }

            let snapshot: Vec<usize> = resolution.enabled.iter().copied().collect();
            for index in snapshot {
                let missing: Vec<String> = self.mods[index]
                    .dependencies
                    .iter()
                    .filter(|dependency| !self.has_enabled_provider(dependency, &resolution.enabled))
                    .cloned()
                    .collect();
                if !missing.is_empty() {
                    resolution.enabled.remove(&index);
                    resolution.missing.insert(index, missing);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        resolution.forced.retain(|index| resolution.enabled.contains(index));
        resolution
    }

    fn has_enabled_provider(&self, dependency: &str, enabled: &BTreeSet<usize>) -> bool {
        let dependency = dependency.to_ascii_lowercase();
        self.providers
            .get(&dependency)
            .is_some_and(|providers| providers.iter().any(|index| enabled.contains(index)))
    }

    fn pick_provider(&self, dependency: &str, enabled: &BTreeSet<usize>) -> Option<usize> {
        let dependency = dependency.to_ascii_lowercase();
        let providers = self.providers.get(&dependency)?;
        providers
            .iter()
            .copied()
            .filter(|index| !enabled.contains(index))
            .filter(|index| !self.mods[*index].blocked)
            // Prefer a jar the user already had switched on; it is the least
            // surprising thing to re-enable for the test.
            .min_by_key(|index| {
                let module = &self.mods[*index];
                (!module.enabled, !module.proven, std::cmp::Reverse(module.modified))
            })
    }

    pub fn key(&self, index: usize) -> &str {
        &self.mods[index].key
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub total: usize,
    pub proven: usize,
    pub unproven: usize,
    pub blocked: Vec<String>,
    pub missing: Vec<String>,
    pub suspects: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestKind {
    Reproduce,
    PoolOnly,
    HalfA,
    HalfB,
    Verify,
}

#[derive(Debug, Clone)]
pub struct Test {
    pub index: usize,
    pub kind: TestKind,
    pub seed: BTreeSet<usize>,
    pub resolution: Resolution,
}

impl Test {
    pub fn enabled_keys(&self, assembly: &Assembly) -> Vec<String> {
        self.resolution
            .enabled
            .iter()
            .map(|&index| assembly.mods[index].key.clone())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    NotReproduced,
    Culprit {
        module: usize,
        also_disabled: Vec<usize>,
        tests: usize,
    },
    Interaction {
        suspects: Vec<usize>,
        tests: usize,
    },
    Inconclusive {
        reason: String,
        tests: usize,
    },
}

#[derive(Debug, Error)]
pub enum BisectError {
    #[error("bisect received a test it did not hand out")]
    UnknownTest,
    #[error("bisect state is already finished")]
    Finished,
}

#[derive(Debug, Clone)]
struct Split {
    a: Vec<usize>,
    b: Vec<usize>,
    tried_a: bool,
    a_result: Option<bool>,
}

#[derive(Debug, Clone)]
enum Stage {
    Reproduce,
    PoolOnly(Vec<usize>),
    Split(Option<Split>),
    Verify(usize),
    Done,
}

#[derive(Debug, Clone)]
pub struct Session {
    assembly: Assembly,
    stage: Stage,
    baseline: BTreeSet<usize>,
    suspects: Vec<usize>,
    original: BTreeSet<usize>,
    tests: usize,
    report: Option<Report>,
}

impl Session {
    pub fn new(assembly: Assembly) -> Self {
        let original = assembly.all_original_enabled();
        Self {
            assembly,
            stage: Stage::Reproduce,
            baseline: BTreeSet::new(),
            suspects: Vec::new(),
            original,
            tests: 0,
            report: None,
        }
    }

    pub fn assembly(&self) -> &Assembly {
        &self.assembly
    }

    pub fn report(&self) -> Option<&Report> {
        self.report.as_ref()
    }

    pub fn tests(&self) -> usize {
        self.tests
    }

    /// First test: reproduce the failure with everything the user had enabled.
    pub fn start(&mut self) -> Test {
        self.tests += 1;
        let seed = self.original.clone();
        let resolution = self.assembly.resolve(&seed);
        Test {
            index: self.tests,
            kind: TestKind::Reproduce,
            seed,
            resolution,
        }
    }

    /// Feed one observation back and get the next test, if any.
    pub fn record(&mut self, test: Test, crashed: bool) -> Result<Option<Test>, BisectError> {
        if self.report.is_some() {
            return Err(BisectError::Finished);
        }
        if test.index != self.tests {
            return Err(BisectError::UnknownTest);
        }
        let stage = self.stage.clone();
        match stage {
            Stage::Reproduce => {
                if !crashed {
                    self.finish(Report::NotReproduced);
                    return Ok(None);
                }
                let pool = self.assembly.unproven();
                let enabled = self.original.len();
                if pool.is_empty() || pool.len() >= enabled {
                    self.suspects = self.original.iter().copied().collect();
                    self.stage = Stage::Split(None);
                } else {
                    self.stage = Stage::PoolOnly(pool);
                }
            }
            Stage::PoolOnly(pool) => {
                if crashed {
                    // Even proven jars that were dragged in as dependencies
                    // are part of this failing set.
                    self.suspects = test
                        .resolution
                        .enabled
                        .iter()
                        .copied()
                        .filter(|index| self.original.contains(index))
                        .collect();
                    self.baseline.clear();
                } else {
                    self.suspects = pool;
                    self.baseline = self
                        .original
                        .iter()
                        .copied()
                        .filter(|index| !self.suspects.contains(index))
                        .collect();
                }
                self.stage = Stage::Split(None);
            }
            Stage::Split(split) => match split {
                None => {}
                Some(mut split) => {
                    if !split.tried_a {
                        if crashed {
                            let next = self.narrow(&test, &split.a);
                            if next == self.suspects {
                                self.finish(Report::Inconclusive {
                                    reason: "the dependency graph prevents a clean split".to_string(),
                                    tests: self.tests,
                                });
                                return Ok(None);
                            }
                            self.suspects = next;
                            self.stage = Stage::Split(None);
                        } else {
                            split.tried_a = true;
                            split.a_result = Some(false);
                            self.stage = Stage::Split(Some(split));
                            return Ok(Some(self.half_test()));
                        }
                    } else if split.a_result == Some(false) {
                        // The B result.
                        if crashed {
                            let next = self.narrow(&test, &split.b);
                            if next == self.suspects {
                                self.finish(Report::Inconclusive {
                                    reason: "the dependency graph prevents a clean split".to_string(),
                                    tests: self.tests,
                                });
                                return Ok(None);
                            }
                            self.suspects = next;
                            self.stage = Stage::Split(None);
                        } else {
                            // Neither half crashes on its own: the failure
                            // needs both sides (or a jar outside the pool).
                            self.finish(Report::Interaction {
                                suspects: self.suspects.clone(),
                                tests: self.tests,
                            });
                            return Ok(None);
                        }
                    }
                }
            },
            Stage::Verify(module) => {
                if crashed {
                    self.finish(Report::Interaction {
                        suspects: vec![module],
                        tests: self.tests,
                    });
                } else {
                    let mut also_disabled: Vec<usize> = self
                        .original
                        .iter()
                        .copied()
                        .filter(|index| *index != module && !test.resolution.enabled.contains(index))
                        .collect();
                    also_disabled.sort_by_key(|&index| self.assembly.mods[index].key.clone());
                    self.finish(Report::Culprit {
                        module,
                        also_disabled,
                        tests: self.tests,
                    });
                }
                return Ok(None);
            }
            Stage::Done => return Ok(None),
        }

        match self.stage.clone() {
            Stage::PoolOnly(pool) => {
                self.tests += 1;
                let seed: BTreeSet<usize> = pool.iter().copied().collect();
                let resolution = self.assembly.resolve(&seed);
                Ok(Some(Test {
                    index: self.tests,
                    kind: TestKind::PoolOnly,
                    seed,
                    resolution,
                }))
            }
            Stage::Split(None) => {
                if self.suspects.len() <= 1 {
                    let Some(module) = self.suspects.first().copied() else {
                        self.finish(Report::Inconclusive {
                            reason: "no suspect is left".to_string(),
                            tests: self.tests,
                        });
                        return Ok(None);
                    };
                    self.stage = Stage::Verify(module);
                    let seed: BTreeSet<usize> =
                        self.original.iter().copied().filter(|index| *index != module).collect();
                    self.tests += 1;
                    let resolution = self.assembly.resolve(&seed);
                    return Ok(Some(Test {
                        index: self.tests,
                        kind: TestKind::Verify,
                        seed,
                        resolution,
                    }));
                }
                let (a, b) = self.split_suspects();
                let split = Split {
                    a,
                    b,
                    tried_a: false,
                    a_result: None,
                };
                self.stage = Stage::Split(Some(split.clone()));
                self.tests += 1;
                let seed: BTreeSet<usize> = self.baseline.iter().copied().chain(split.a.iter().copied()).collect();
                let resolution = self.assembly.resolve(&seed);
                Ok(Some(Test {
                    index: self.tests,
                    kind: TestKind::HalfA,
                    seed,
                    resolution,
                }))
            }
            _ => Ok(None),
        }
    }

    fn half_test(&mut self) -> Test {
        let Stage::Split(Some(split)) = self.stage.clone() else {
            unreachable!("half_test without a split");
        };
        self.tests += 1;
        let seed: BTreeSet<usize> = self.baseline.iter().copied().chain(split.b.iter().copied()).collect();
        let resolution = self.assembly.resolve(&seed);
        Test {
            index: self.tests,
            kind: TestKind::HalfB,
            seed,
            resolution,
        }
    }

    fn narrow(&mut self, test: &Test, half: &[usize]) -> Vec<usize> {
        let mut next: Vec<usize> = self
            .suspects
            .iter()
            .copied()
            .filter(|index| half.contains(index) && test.resolution.enabled.contains(index))
            .collect();
        // If the resolution forced the other half back on, the split proved
        // nothing. Keep the full suspect list so the caller stops safely.
        if next.is_empty() {
            next = self.suspects.clone();
        }
        next
    }

    fn split_suspects(&self) -> (Vec<usize>, Vec<usize>) {
        let middle = self.suspects.len() / 2;
        (self.suspects[..middle].to_vec(), self.suspects[middle..].to_vec())
    }

    fn finish(&mut self, report: Report) {
        self.report = Some(report);
        self.stage = Stage::Done;
    }
}

impl fmt::Display for TestKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Reproduce => "reproduce",
            Self::PoolOnly => "new mods only",
            Self::HalfA => "first half",
            Self::HalfB => "second half",
            Self::Verify => "verify",
        };
        f.write_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(key: &str, dependencies: &[&str], enabled: bool, proven: bool, modified: u64) -> Mod {
        Mod {
            key: key.to_string(),
            id: key.to_string(),
            version: "1.0".to_string(),
            enabled,
            proven,
            blocked: false,
            modified,
            dependencies: dependencies.iter().map(|dep| dep.to_string()).collect(),
            provides: Vec::new(),
            nested: Vec::new(),
        }
    }

    fn drive(assembly: Assembly, crashes: impl Fn(&BTreeSet<usize>) -> bool) -> (Report, usize, Vec<Test>) {
        let mut session = Session::new(assembly);
        let mut test = session.start();
        let mut seen = vec![test.clone()];
        loop {
            let crashed = crashes(&test.resolution.enabled);
            match session.record(test.clone(), crashed).expect("record") {
                Some(next) => {
                    seen.push(next.clone());
                    test = next;
                }
                None => break,
            }
        }
        let report = session.report().cloned().expect("report");
        (report, seen.len(), seen)
    }

    #[test]
    fn finds_a_single_new_bad_mod() {
        let assembly = Assembly::new(vec![
            module("old", &[], true, true, 1),
            module("new", &[], true, false, 30),
            module("other", &[], true, false, 20),
        ]);
        let new_index = 1;
        let (report, tests, _) = drive(assembly, |enabled| enabled.contains(&new_index));
        assert!(matches!(
            report,
            Report::Culprit { module, .. } if module == new_index
        ));
        assert!(tests >= 3);
    }

    #[test]
    fn new_mod_that_needs_an_old_mod_is_still_found() {
        let assembly = Assembly::new(vec![
            module("old", &[], true, true, 1),
            module("new", &["old"], true, false, 30),
            module("other", &[], true, false, 20),
        ]);
        let new_index = 1;
        let (report, _, _) = drive(assembly, |enabled| enabled.contains(&new_index) && enabled.contains(&0));
        assert!(
            matches!(report, Report::Culprit { module, .. } if module == new_index),
            "got {report:?}"
        );
    }

    #[test]
    fn disabling_a_library_disables_its_dependents() {
        let assembly = Assembly::new(vec![
            module("lib", &[], true, true, 1),
            module("addon", &["lib"], true, false, 10),
            module("unrelated", &[], true, false, 5),
        ]);
        let mut seed = BTreeSet::new();
        seed.insert(1);
        let resolution = assembly.resolve(&seed);
        assert!(resolution.enabled.contains(&0), "the dependency is re-enabled");
        assert!(resolution.enabled.contains(&1));
        assert!(resolution.forced.contains(&0));

        // Asking for the addon alone keeps it; asking for the addon while the
        // library is impossible drops it too.
        let mut seed = BTreeSet::new();
        seed.insert(1);
        let mut blocked = assembly.clone();
        blocked.mods[0].blocked = true;
        let blocked = Assembly::new(blocked.mods);
        let resolution = blocked.resolve(&seed);
        assert!(!resolution.enabled.contains(&1), "the dependent is dropped");
        assert!(resolution.missing.contains_key(&1));
    }

    #[test]
    fn pair_interaction_is_reported_not_guessed() {
        let assembly = Assembly::new(vec![
            module("old", &[], true, true, 1),
            module("a", &[], true, false, 30),
            module("b", &[], true, false, 20),
        ]);
        let (a, b) = (1, 2);
        let (report, _, _) = drive(assembly, |enabled| enabled.contains(&a) && enabled.contains(&b));
        assert!(
            matches!(report, Report::Interaction { ref suspects, .. } if suspects.contains(&a) || suspects.contains(&b)),
            "got {report:?}"
        );
    }

    #[test]
    fn pool_only_pass_falls_back_to_the_proven_baseline() {
        let assembly = Assembly::new(vec![
            module("old", &[], true, true, 1),
            module("new1", &[], true, false, 30),
            module("new2", &[], true, false, 20),
        ]);
        // The crash needs the old jar plus new1. Testing the new jars alone
        // passes, so the old jar becomes the baseline.
        let (report, _, tests) = drive(assembly, |enabled| enabled.contains(&0) && enabled.contains(&1));
        assert!(
            matches!(report, Report::Culprit { module, .. } if module == 1),
            "got {report:?}"
        );
        assert!(tests.iter().any(|test| test.kind == TestKind::PoolOnly));
    }

    #[test]
    fn every_mod_proven_still_bisects_the_whole_set() {
        let assembly = Assembly::new(vec![
            module("a", &[], true, true, 1),
            module("b", &[], true, true, 2),
            module("c", &[], true, true, 3),
            module("d", &[], true, true, 4),
        ]);
        let (report, _, _) = drive(assembly, |enabled| enabled.contains(&2));
        assert!(
            matches!(report, Report::Culprit { module, .. } if module == 2),
            "got {report:?}"
        );
    }

    #[test]
    fn no_crash_means_not_reproduced() {
        let assembly = Assembly::new(vec![module("a", &[], true, false, 1)]);
        let (report, tests, _) = drive(assembly, |_| false);
        assert_eq!(report, Report::NotReproduced);
        assert_eq!(tests, 1);
    }

    #[test]
    fn proven_mods_are_not_the_first_pool() {
        let assembly = Assembly::new(vec![
            module("old", &[], true, true, 1),
            module("new", &[], true, false, 30),
        ]);
        let plan = assembly.plan();
        assert_eq!(plan.proven, 1);
        assert_eq!(plan.unproven, 1);
        assert_eq!(plan.suspects, vec!["new".to_string()]);
    }
}
