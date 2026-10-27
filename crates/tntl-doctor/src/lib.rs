//! Offline instance checks and log analysis.
//!
//! Both paths produce [`Finding`] values with a stable `code`. The UI maps the
//! code to a localized title, detail template and hint, so this crate never
//! carries user-facing prose.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod log;
pub mod mods;
pub mod version;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// One problem, either found while inspecting files or while reading a log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    /// Stable identifier used by the UI for the localized text.
    pub code: String,
    /// The mod, file or component the finding is about.
    #[serde(default)]
    pub subject: Option<String>,
    /// Values filled into the localized detail template.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Raw log line or file path that proves the finding.
    #[serde(default)]
    pub evidence: Option<String>,
    /// Zero-based line index in the analyzed log.
    #[serde(default)]
    pub line: Option<usize>,
}

impl Finding {
    pub fn new(severity: Severity, code: &str) -> Self {
        Self {
            severity,
            code: code.to_string(),
            subject: None,
            params: BTreeMap::new(),
            evidence: None,
            line: None,
        }
    }

    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        let subject = subject.into();
        if !subject.is_empty() {
            self.subject = Some(subject);
        }
        self
    }

    pub fn param(mut self, key: &str, value: impl Into<String>) -> Self {
        self.params.insert(key.to_string(), value.into());
        self
    }

    pub fn evidence(mut self, evidence: impl Into<String>) -> Self {
        let evidence = evidence.into();
        if !evidence.is_empty() {
            self.evidence = Some(truncate(&evidence, 600));
        }
        self
    }

    pub fn line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }
}

/// Keep the newest findings only, drop duplicates and put errors first.
pub fn tidy(mut findings: Vec<Finding>) -> Vec<Finding> {
    let mut seen = std::collections::HashSet::new();
    findings.retain(|finding| {
        seen.insert((
            finding.code.clone(),
            finding.subject.clone(),
            finding.params.clone(),
            finding.evidence.clone(),
        ))
    });
    findings.sort_by(|left, right| {
        left.severity
            .cmp(&right.severity)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.subject.cmp(&right.subject))
    });
    findings.truncate(60);
    findings
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_drops_exact_duplicates_and_sorts_errors_first() {
        let warn = Finding::new(Severity::Warning, "b");
        let error = Finding::new(Severity::Error, "a");
        let findings = tidy(vec![warn.clone(), error.clone(), warn]);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].severity, Severity::Error);
    }
}
