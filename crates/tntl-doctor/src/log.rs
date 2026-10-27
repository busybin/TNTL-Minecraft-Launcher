//! Log scanning: per-line levels plus plain-language findings.
//!
//! Levels drive the viewer's filters. Findings are matched with stable codes;
//! the UI turns each code into localized text and shows the matched line as
//! evidence.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Finding, Severity, tidy};

/// Debug/trace stay hidden by default. `Context` marks stack frames and
/// `Caused by` lines, which belong to the warning or error above them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
    Context,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub total: usize,
    pub debug: usize,
    pub info: usize,
    pub warn: usize,
    pub error: usize,
    pub context: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogAnalysis {
    pub counts: Counts,
    /// One entry per input line.
    pub levels: Vec<Level>,
    pub findings: Vec<Finding>,
}

/// Analyze a log or crash report. The input is capped by the caller; this
/// function only walks the text once per rule.
pub fn analyze(text: &str) -> LogAnalysis {
    let lines: Vec<&str> = text.lines().collect();
    let lower: Vec<String> = lines.iter().map(|line| line.to_ascii_lowercase()).collect();
    let (levels, counts) = classify(&lines);
    let mut findings = collect(&lines, &lower, &counts);

    // The specific diagnosis is more useful than the generic fallback.
    if findings.iter().any(|finding| finding.severity == Severity::Error) {
        findings.retain(|finding| finding.code != "crash_report");
    }
    if findings.is_empty()
        && counts.error > 0
        && let Some(index) = levels.iter().position(|level| *level == Level::Error)
    {
        findings.push(
            Finding::new(Severity::Warning, "unknown_crash")
                .evidence(lines[index])
                .line(index),
        );
    }
    LogAnalysis {
        counts,
        levels,
        findings: tidy(findings),
    }
}

fn xml_event_level(line: &str) -> Option<Level> {
    if !line.contains("<log4j:Event") {
        return None;
    }
    let lower = line.to_ascii_lowercase();
    for (value, level) in [
        ("level=\"fatal\"", Level::Error),
        ("level=\"error\"", Level::Error),
        ("level=\"warn\"", Level::Warn),
        ("level=\"debug\"", Level::Debug),
        ("level=\"trace\"", Level::Debug),
        ("level=\"info\"", Level::Info),
        ("level='error'", Level::Error),
        ("level='warn'", Level::Warn),
        ("level='debug'", Level::Debug),
        ("level='info'", Level::Info),
    ] {
        if lower.contains(value) {
            return Some(level);
        }
    }
    None
}

fn classify(lines: &[&str]) -> (Vec<Level>, Counts) {
    let mut levels = Vec::with_capacity(lines.len());
    let mut counts = Counts {
        total: lines.len(),
        ..Counts::default()
    };
    let mut problem = false;
    let mut event: Option<Level> = None;
    for line in lines {
        let level = if let Some(level) = xml_event_level(line) {
            event = Some(level);
            problem = matches!(level, Level::Warn | Level::Error);
            level
        } else if event.is_some() && line.trim_start().starts_with("<log4j:Message") {
            event.unwrap_or(Level::Info)
        } else if line.contains("</log4j:Event>") {
            event = None;
            problem = false;
            Level::Info
        } else if line.trim().is_empty() {
            problem = false;
            Level::Info
        } else if let Some(level) = marker_level(line) {
            problem = matches!(level, Level::Warn | Level::Error);
            level
        } else if is_continuation(line) {
            if problem { Level::Context } else { Level::Info }
        } else {
            problem = false;
            Level::Info
        };
        match level {
            Level::Debug => counts.debug += 1,
            Level::Info => counts.info += 1,
            Level::Warn => counts.warn += 1,
            Level::Error => counts.error += 1,
            Level::Context => counts.context += 1,
        }
        levels.push(level);
    }
    (levels, counts)
}

fn marker_level(line: &str) -> Option<Level> {
    for (needle, level) in [
        ("/FATAL]", Level::Error),
        ("/ERROR]", Level::Error),
        ("/WARN]", Level::Warn),
        ("/DEBUG]", Level::Debug),
        ("/TRACE]", Level::Debug),
        ("/INFO]", Level::Info),
    ] {
        if line.contains(needle) {
            return Some(level);
        }
    }
    let trimmed = line.trim_start();
    for (prefix, level) in [
        ("[FATAL]", Level::Error),
        ("[ERROR]", Level::Error),
        ("[WARN]", Level::Warn),
        ("[WARNING]", Level::Warn),
        ("[DEBUG]", Level::Debug),
        ("[TRACE]", Level::Debug),
        ("[INFO]", Level::Info),
        ("FATAL:", Level::Error),
        ("ERROR:", Level::Error),
        ("WARN:", Level::Warn),
    ] {
        if trimmed.starts_with(prefix) {
            return Some(level);
        }
    }
    if trimmed.starts_with("---- Minecraft Crash Report ----")
        || trimmed.contains("EXCEPTION_ACCESS_VIOLATION")
        || trimmed.starts_with("SIGSEGV")
        || trimmed.starts_with("# A fatal error")
        || trimmed.starts_with("A fatal error has been detected")
    {
        return Some(Level::Error);
    }
    if trimmed.starts_with("Exception in thread") {
        return Some(Level::Error);
    }
    let looks_like_class = ["java.", "javax.", "net.", "org.", "com."]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix));
    if looks_like_class && (trimmed.contains("Exception") || trimmed.contains("Error")) {
        return Some(Level::Error);
    }
    None
}

fn is_continuation(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("Caused by:") || trimmed.starts_with("Suppressed:") || trimmed.starts_with("...") {
        return true;
    }
    trimmed.len() != line.len()
        && (trimmed.starts_with("at ")
            || trimmed.starts_with("-- ")
            || trimmed.starts_with("Short stack trace")
            || trimmed.starts_with("Stacktrace:"))
}

fn collect(lines: &[&str], lower: &[String], counts: &Counts) -> Vec<Finding> {
    let mut findings = Vec::new();

    if let Some(index) = first(lower, &["outofmemoryerror", "out of memory"]) {
        findings.push(
            Finding::new(Severity::Error, "out_of_memory")
                .evidence(lines[index])
                .line(index),
        );
    } else if let Some(index) = first(lower, &["unsupportedclassversionerror", "unsupported class file"]) {
        let required = class_version_major(lines[index]).map(|major| major.to_string());
        let mut finding = Finding::new(Severity::Error, "java_too_old")
            .evidence(lines[index])
            .line(index);
        if let Some(required) = required {
            finding = finding.param("required", required);
        }
        findings.push(finding);
    }

    if let Some(index) = first(
        lower,
        &["could not find or load main class", "could not find main class"],
    ) {
        let name = after(lines[index], "main class").unwrap_or_default();
        let mut finding = Finding::new(Severity::Error, "main_class")
            .evidence(lines[index])
            .line(index);
        if !name.is_empty() {
            finding = finding.subject(name);
        }
        findings.push(finding);
    }

    if let Some(index) = first(
        lower,
        &[
            "unrecognized vm option",
            "unrecognized option:",
            "could not create the java virtual machine",
        ],
    ) {
        let mut finding = Finding::new(Severity::Error, "jvm_args")
            .evidence(lines[index])
            .line(index);
        if let Some(option) = quoted(lines[index]) {
            finding = finding.param("arg", option);
        }
        findings.push(finding);
    }

    findings.extend(missing_dependencies(lines, lower));
    findings.extend(mod_file_findings(lines, lower));
    findings.extend(mod_runtime_findings(lines, lower));
    if findings.iter().all(|finding| finding.code != "mod_crash")
        && lower
            .iter()
            .any(|line| line.contains("---- minecraft crash report ----"))
        && let Some(index) = lines.iter().position(|line| {
            let text = line.trim_start();
            text.starts_with("java.") && (text.contains("Exception") || text.contains("Error"))
        })
    {
        let culprit = failing_mod(lines);
        let message = culprit
            .as_ref()
            .map(|(_, _, message)| message.trim())
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| lines[index].trim());
        let mut finding = Finding::new(Severity::Error, "mod_crash")
            .param("exception", message.to_string())
            .evidence(lines[index])
            .line(index);
        if let Some((_, id, _)) = culprit {
            finding = finding.subject(id);
        }
        findings.push(finding);
    }

    if let Some(index) = first(
        lower,
        &[
            "exception_access_violation",
            "a fatal error has been detected by the java runtime environment",
            "sigsegv",
            "hs_err_pid",
            "problematic frame",
        ],
    ) {
        findings.push(
            Finding::new(Severity::Error, "native_crash")
                .evidence(lines[index])
                .line(index),
        );
    }

    if let Some(index) = first(
        lower,
        &[
            "failed to create window",
            "glfw error",
            "pixel format not accelerated",
            "could not create gl context",
            "lwjglexception",
            "the driver does not appear to support opengl",
            "wglcreatecontext failed",
        ],
    ) {
        findings.push(
            Finding::new(Severity::Warning, "graphics")
                .evidence(lines[index])
                .line(index),
        );
    }

    if let Some(index) = first(
        lower,
        &[
            "failed to authenticate",
            "invalid session",
            "authentication servers are down",
            "failed to login",
        ],
    ) {
        findings.push(
            Finding::new(Severity::Warning, "auth")
                .evidence(lines[index])
                .line(index),
        );
    }

    if let Some(index) = first(
        lower,
        &[
            "unknownhostexception",
            "connectexception",
            "connection refused",
            "read timed out",
            "sockettimeout",
            "network is unreachable",
        ],
    ) {
        findings.push(
            Finding::new(Severity::Warning, "network")
                .evidence(lines[index])
                .line(index),
        );
    }

    // Mods mention OptiFine in compatibility probes ("No Optifine!",
    // "Skipping mod compatibility accessor for: [optifine]"), so only a real
    // install (jar name or version header) counts as evidence.
    if let Some(index) = first(lower, &["optifine_", "optifine version"]) {
        let others: Vec<&str> = ["sodium", "iris", "embeddium", "rubidium", "oculus"]
            .into_iter()
            .filter(|name| lower.iter().any(|line| line.contains(name)))
            .collect();
        if !others.is_empty() {
            findings.push(
                Finding::new(Severity::Warning, "optifine_conflict")
                    .param("mods", others.join(" + "))
                    .evidence(lines[index])
                    .line(index),
            );
        }
    }

    if let Some(index) = first(lower, &["---- minecraft crash report ----"]) {
        let description = lines
            .iter()
            .find_map(|line| {
                let (head, tail) = line.split_once(':')?;
                head.trim()
                    .eq_ignore_ascii_case("description")
                    .then(|| tail.trim().to_string())
            })
            .unwrap_or_default();
        let mut finding = Finding::new(Severity::Info, "crash_report")
            .evidence(lines[index])
            .line(index);
        if !description.is_empty() {
            finding = finding.param("description", description);
        }
        findings.push(finding);
    } else if counts.error > 0
        && let Some(index) = first(lower, &["caused by:"])
        && lower.iter().any(|line| line.starts_with("description:"))
    {
        findings.push(
            Finding::new(Severity::Info, "crash_report")
                .evidence(lines[index])
                .line(index),
        );
    }

    findings
}

fn missing_dependencies(lines: &[&str], lower: &[String]) -> Vec<Finding> {
    let triggered = lower.iter().any(|line| {
        line.contains("incompatible mod set")
            || line.contains("mod resolution failed")
            || line.contains("mod resolution encountered")
            || line.contains("missing or unsupported mandatory dependencies")
            || line.contains("which is missing")
    });
    if !triggered {
        return Vec::new();
    }
    let mut requests: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in lines {
        if let Some((module, dependency)) = fabric_missing(line).or_else(|| forge_missing(line)) {
            requests.entry(module).or_default().push(dependency);
        }
    }
    if requests.is_empty() {
        let index = first(
            lower,
            &[
                "incompatible mod set",
                "mod resolution failed",
                "missing or unsupported mandatory dependencies",
            ],
        )
        .unwrap_or_default();
        return vec![
            Finding::new(Severity::Error, "mod_resolution")
                .evidence(lines[index])
                .line(index),
        ];
    }
    requests
        .into_iter()
        .map(|(module, mut dependencies)| {
            dependencies.sort();
            dependencies.dedup();
            let index = lines.iter().position(|line| line.contains(&module)).unwrap_or_default();
            FindingsBuilder::new(Severity::Error, "missing_dependency")
                .subject(module)
                .param("deps", dependencies.join(", "))
                .evidence(lines[index])
                .line(index)
                .build()
        })
        .collect()
}

struct FindingsBuilder(Finding);

impl FindingsBuilder {
    fn new(severity: Severity, code: &str) -> Self {
        Self(Finding::new(severity, code))
    }

    fn subject(mut self, subject: impl Into<String>) -> Self {
        self.0 = self.0.subject(subject);
        self
    }

    fn param(mut self, key: &str, value: impl Into<String>) -> Self {
        self.0 = self.0.param(key, value);
        self
    }

    fn evidence(mut self, evidence: impl Into<String>) -> Self {
        self.0 = self.0.evidence(evidence);
        self
    }

    fn line(mut self, line: usize) -> Self {
        self.0 = self.0.line(line);
        self
    }

    fn build(self) -> Finding {
        self.0
    }
}

fn fabric_missing(line: &str) -> Option<(String, String)> {
    if !line.contains("which is missing") {
        return None;
    }
    let module = between(line, "Mod '", "'")?;
    let after = line.split_once("requires ")?.1;
    let dependency = match after.split_once(" of ") {
        Some((_, tail)) => tail,
        None => after,
    };
    let dependency = dependency
        .split([',', '!', ';'])
        .next()?
        .trim()
        .trim_matches(['\'', '"', '.']);
    if dependency.is_empty() || dependency.contains(' ') {
        return None;
    }
    Some((module, dependency.to_string()))
}

fn forge_missing(line: &str) -> Option<(String, String)> {
    let dependency = between(line, "Mod ID: '", "'")?;
    let module = between(line, "Requested by: '", "'")?;
    Some((module, dependency))
}

fn mod_file_findings(lines: &[&str], lower: &[String]) -> Vec<Finding> {
    let mut findings = Vec::new();
    if let Some(index) = first(lower, &["constructed 0 mods", "has mods that were not found"]) {
        let file = jar_name(lines[index]).unwrap_or_else(|| lines[index].trim().to_string());
        findings.push(
            Finding::new(Severity::Error, "unloadable_mod")
                .subject(file)
                .evidence(lines[index])
                .line(index),
        );
    }
    if let Some(index) = first(
        lower,
        &[
            "is not a valid mod file",
            "invalid mod file",
            "fabric.mod.json could not be found",
            "could not find fabric.mod.json",
            "mods.toml could not be found",
            "does not contain a mod",
        ],
    ) {
        let file = jar_name(lines[index]).unwrap_or_else(|| lines[index].trim().to_string());
        findings.push(
            Finding::new(Severity::Error, "invalid_mod")
                .subject(file)
                .evidence(lines[index])
                .line(index),
        );
    }
    if let Some(index) = first(
        lower,
        &[
            "zipexception",
            "zip end header not found",
            "invalid cen header",
            "invalidjar",
            "invalid jar",
        ],
    ) {
        let file = jar_name(lines[index]).unwrap_or_else(|| lines[index].trim().to_string());
        findings.push(
            Finding::new(Severity::Error, "corrupt_mod")
                .subject(file)
                .evidence(lines[index])
                .line(index),
        );
    }
    if let Some(index) = first(lower, &["duplicate mod", "duplicate mod id"]) {
        findings.push(
            Finding::new(Severity::Error, "duplicate_mod")
                .evidence(lines[index])
                .line(index),
        );
    }
    findings
}

fn mod_runtime_findings(lines: &[&str], lower: &[String]) -> Vec<Finding> {
    let platform = lower.iter().any(|line| {
        line.contains("fabricloader")
            || line.contains("forge")
            || line.contains("neoforge")
            || line.contains("quilt_loader")
            || line.contains("mixin")
    });
    let mut findings = Vec::new();

    if let Some(index) = first(
        lower,
        &[
            "mixin apply for mod",
            "mixintransformererror",
            "critical injection failure",
            "injectionerror",
            "mixin apply failed",
        ],
    ) {
        let module = between(lines[index], "Mixin apply for mod", " failed")
            .map(|name| name.trim().to_string())
            .unwrap_or_default();
        let mut finding = Finding::new(Severity::Error, "mixin_failed")
            .evidence(lines[index])
            .line(index);
        if !module.is_empty() {
            finding = finding.subject(module);
        }
        findings.push(finding);
    }

    let modded = platform
        || lower
            .iter()
            .any(|line| line.contains("net.minecraft") || line.contains("mods/"));
    if platform
        && let Some(index) = first(
            lower,
            &[
                "exception caught during firing event",
                "failed to create mod instance",
                "error during pre-loading",
                "mod initialization failed",
                "exception in thread \"main\"",
            ],
        )
    {
        findings.push(
            Finding::new(Severity::Error, "mod_crash")
                .param("exception", lines[index].trim().to_string())
                .evidence(lines[index])
                .line(index),
        );
    }
    if modded {
        if let Some(index) = first(
            lower,
            &[
                "nosuchmethoderror",
                "nosuchfielderror",
                "abstractmethoderror",
                "incompatibleclasschangeerror",
            ],
        ) {
            findings.push(
                Finding::new(Severity::Error, "version_mismatch")
                    .evidence(lines[index])
                    .line(index),
            );
        }
        let missing_class = first(lower, &["noclassdeffounderror"]).or_else(|| {
            lower.iter().enumerate().find_map(|(index, line)| {
                (line.contains("classnotfoundexception") && !benign_missing(line)).then_some(index)
            })
        });
        if let Some(index) = missing_class {
            let class = lines[index]
                .split_once(": ")
                .map(|(_, tail)| tail.trim().to_string())
                .filter(|tail| !tail.is_empty());
            let mut finding = Finding::new(Severity::Error, "missing_class")
                .evidence(lines[index])
                .line(index);
            if let Some(class) = class {
                finding = finding.param("class", class);
            }
            findings.push(finding);
        }
    }
    findings
}

/// Forge/NeoForge crash reports put the offending mod in a `-- MOD id --`
/// section, directly above the failure message.
fn failing_mod(lines: &[&str]) -> Option<(usize, String, String)> {
    let start = lines.iter().position(|line| {
        let text = line.trim();
        text.starts_with("-- MOD ") && text.ends_with("--")
    })?;
    let id = lines[start]
        .trim()
        .trim_start_matches("-- MOD ")
        .trim_end_matches("--")
        .trim()
        .to_string();
    let message = lines
        .iter()
        .skip(start)
        .take(12)
        .find_map(|line| line.trim().strip_prefix("Failure message:"))
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    Some((start, id, message))
}

fn first(lower: &[String], needles: &[&str]) -> Option<usize> {
    lower
        .iter()
        .position(|line| needles.iter().any(|needle| line.contains(needle)))
}

fn between(text: &str, start: &str, end: &str) -> Option<String> {
    let rest = text.split_once(start)?.1;
    let value = rest.split_once(end)?.0.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn after(text: &str, marker: &str) -> Option<String> {
    let rest = text.split_once(marker)?.1.trim();
    let value = rest.trim_matches(['\'', '"', ':', ' ', '.']);
    (!value.is_empty()).then(|| value.to_string())
}

fn quoted(text: &str) -> Option<String> {
    let start = text.find(['\'', '"'])?;
    let rest = &text[start + 1..];
    let end = rest.find(['\'', '"'])?;
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn jar_name(line: &str) -> Option<String> {
    line.split(|ch: char| ch.is_whitespace() || matches!(ch, '\'' | '"' | ',' | ';' | '[' | ']' | '(' | ')'))
        .find(|token| token.contains(".jar"))
        .map(|token| token.rsplit(['/', '\\']).next().unwrap_or(token).to_string())
}

fn class_version_major(line: &str) -> Option<u32> {
    let start = line.find("class file version ")? + "class file version ".len();
    let rest = &line[start..];
    let value: u32 = rest.split(|ch: char| !ch.is_ascii_digit()).next()?.parse().ok()?;
    (value >= 49).then_some(value - 44)
}

/// Mixin probes optional targets and Forge scans annotations; a missing
/// IntelliJ annotation is noise, not a broken mod.
fn benign_missing(line: &str) -> bool {
    line.contains("error loading class")
        || [
            "org/jetbrains/annotations",
            "org/jetbrains/",
            "org/intellij/",
            "kotlin/",
        ]
        .iter()
        .any(|prefix| line.contains(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(text: &str) -> Vec<String> {
        analyze(text).findings.into_iter().map(|finding| finding.code).collect()
    }

    #[test]
    fn levels_follow_the_log_markers() {
        let text = "[12:00:00] [main/INFO] [minecraft/Main]: start\n\
                    [12:00:01] [main/WARN] [minecraft/Foo]: careful\n\
                    [12:00:02] [main/ERROR] [minecraft/Bar]: boom\n\
                    \tat com.example.Bar.call(Bar.java:1)\n\
                    [12:00:03] [main/DEBUG] [minecraft/Baz]: detail";
        let analysis = analyze(text);
        assert_eq!(analysis.counts.warn, 1);
        assert_eq!(analysis.counts.error, 1);
        assert_eq!(analysis.counts.context, 1);
        assert_eq!(analysis.levels[0], Level::Info);
        assert_eq!(analysis.levels[3], Level::Context);
        assert_eq!(analysis.levels[4], Level::Debug);
    }

    #[test]
    fn finds_fabric_missing_dependencies() {
        let text = "[12:00:00] [main/ERROR] [fabricloader/]: Incompatible mod set!\n\
                    \t- Mod 'Iris' (iris) 1.7.3 requires version 0.92.0 or later of fabric-api, which is missing!\n\
                    \t- Mod 'Sodium Extra' (sodium-extra) 0.5.4 requires any version of sodium, which is missing!";
        let analysis = analyze(text);
        let missing: Vec<_> = analysis
            .findings
            .iter()
            .filter(|finding| finding.code == "missing_dependency")
            .collect();
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[0].params.get("deps"), Some(&"fabric-api".to_string()));
        assert_eq!(missing[0].subject.as_deref(), Some("Iris"));
    }

    #[test]
    fn finds_forge_missing_dependencies() {
        let text = "Missing or unsupported mandatory dependencies:\n\
                    \tMod ID: 'curios', Requested by: 'example', Expected range: '[5.2.0,)', Actual version: '[MISSING]'";
        let analysis = analyze(text);
        assert!(
            analysis
                .findings
                .iter()
                .any(|finding| finding.code == "missing_dependency"
                    && finding.params.get("deps").is_some_and(|deps| deps.contains("curios")))
        );
    }

    #[test]
    fn finds_common_crashes() {
        assert!(codes("java.lang.OutOfMemoryError: Java heap space").contains(&"out_of_memory".to_string()));
        assert!(
            codes("java.lang.UnsupportedClassVersionError: x (class file version 65.0)")
                .contains(&"java_too_old".to_string())
        );
        assert!(
            codes("Mixin apply for mod example failed example.mixins.json:ExampleMixin")
                .contains(&"mixin_failed".to_string())
        );
        assert!(
            codes("[main/ERROR] [fabricloader/]: Mod file /mods/foo.jar is not a valid mod file")
                .contains(&"invalid_mod".to_string())
        );
        assert!(
            codes(
                "[main/FATAL] [ne.mi.fm.ModLoader/LOADING]: File /mods/ImmediatelyFast-NeoForge-1.5.5+1.20.4.jar constructed 0 mods: [], but had 1 mods specified: [immediatelyfast]"
            )
            .contains(&"unloadable_mod".to_string())
        );
        assert!(
            codes("java.lang.NoSuchMethodError: net.minecraft.client.Main.run()V")
                .contains(&"version_mismatch".to_string())
        );
    }

    #[test]
    fn crash_report_names_the_failing_mod() {
        let text = "---- Minecraft Crash Report ----\n\
                    Description: Mod loading error has occurred\n\
                    java.lang.Exception: Mod Loading has failed\n\
                    \tat net.minecraftforge.logging.CrashReportExtender.dump(CrashReportExtender.java:60)\n\
                    \n\
                    -- MOD kacchi --\n\
                    Details:\n\
                    \tMod File: /home/x/mods/kacchi-2.0.0-1.20.1.jar\n\
                    \tFailure message: Farmer's Delight: Kacchi (kacchi) has failed to load correctly\n\
                    \t\tjava.lang.RuntimeException: Could not find mod constructor.";
        let analysis = analyze(text);
        let finding = analysis
            .findings
            .iter()
            .find(|finding| finding.code == "mod_crash")
            .expect("expected a mod crash");
        assert_eq!(finding.subject.as_deref(), Some("kacchi"));
        assert!(
            finding
                .params
                .get("exception")
                .is_some_and(|value| value.contains("failed to load correctly"))
        );
    }

    #[test]
    fn optifine_mentions_are_not_a_conflict() {
        // Distant Horizons and Xaero probe for OptiFine on every start.
        let text = "[12:00:00] [Worker/INFO] [DistantHorizons/]: Skipping mod compatibility accessor for: [optifine]\n\
                    [12:00:01] [Render thread/INFO] [xaero.map.WorldMap/]: No Optifine!\n\
                    [12:00:02] [Render thread/INFO] [embeddium/]: Embeddium loaded\n\
                    [12:00:03] [Render thread/INFO] [oculus/]: Oculus loaded";
        assert!(!codes(text).contains(&"optifine_conflict".to_string()));
    }

    #[test]
    fn optifine_jar_next_to_oculus_is_a_conflict() {
        let text = "OptiFine_1.20.1_HD_U_I6.jar |OptiFine|optifine|1.20.1_HD_U_I6|COMMON_SET\n\
                    oculus-mc1.20.1-1.8.0.jar |Oculus|oculus|1.8.0|COMMON_SET";
        let analysis = analyze(text);
        let finding = analysis
            .findings
            .iter()
            .find(|finding| finding.code == "optifine_conflict")
            .expect("expected an OptiFine conflict");
        assert_eq!(finding.params.get("mods"), Some(&"oculus".to_string()));
    }

    #[test]
    fn clean_logs_have_no_findings() {
        let text = "[12:00:00] [main/INFO] [minecraft/Main]: Setting user\n\
                    [12:00:01] [main/INFO] [minecraft/Main]: Backend library: LWJGL\n\
                    [12:00:02] [main/WARN] [minecraft/Sound]: Sound engine paused";
        assert!(analyze(text).findings.is_empty());
    }

    #[test]
    fn xml_events_keep_their_level() {
        let text = "<log4j:Event logger=\"mixin\" level=\"WARN\" thread=\"main\">\n\
                    <log4j:Message><![CDATA[Error loading class: org/jetbrains/annotations/ApiStatus (java.lang.ClassNotFoundException: x)]]></log4j:Message>\n\
                    </log4j:Event>";
        let analysis = analyze(text);
        assert_eq!(analysis.counts.error, 0);
        assert_eq!(analysis.counts.warn, 2);
        assert!(!analysis.findings.iter().any(|finding| finding.code == "missing_class"));
    }

    #[test]
    fn crash_report_names_the_first_exception() {
        let text = "---- Minecraft Crash Report ----\n\
                    // Why did you do that?\n\
                    Time: 2026-01-01\n\
                    Description: Rendering overlay\n\
                    java.lang.NullPointerException: Cannot invoke foo\n\
                    \tat com.example.Mod.render(Mod.java:1)";
        let analysis = analyze(text);
        assert!(analysis.findings.iter().any(|finding| {
            finding.code == "mod_crash"
                && finding
                    .params
                    .get("exception")
                    .is_some_and(|value| value.contains("NullPointerException"))
        }));
    }
}
