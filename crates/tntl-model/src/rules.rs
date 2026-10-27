use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::env::Environment;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    #[default]
    Allow,
    Disallow,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OsRule {
    pub name: Option<String>,
    pub version: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub action: RuleAction,
    pub os: Option<OsRule>,
    pub features: Option<std::collections::BTreeMap<String, bool>>,
}

impl Rule {
    pub fn applies(&self, env: &Environment) -> bool {
        if let Some(os) = &self.os {
            if let Some(name) = &os.name
                && name != env.os_name()
            {
                return false;
            }
            if let Some(arch) = &os.arch
                && arch != env.arch.as_str()
            {
                return false;
            }
            if let Some(pattern) = &os.version {
                let Some(actual) = &env.os_version else {
                    return false;
                };
                match Regex::new(pattern) {
                    Ok(re) => {
                        if !re.is_match(actual) {
                            return false;
                        }
                    }
                    Err(_) => return false,
                }
            }
        }
        if let Some(features) = &self.features {
            for (name, wanted) in features {
                let actual = env.features.get(name).copied().unwrap_or(false);
                if actual != *wanted {
                    return false;
                }
            }
        }
        true
    }
}

/// Evaluate a rule list. An empty list allows the entry; otherwise the last
/// matching rule wins and the default is disallowed.
pub fn allowed(rules: &[Rule], env: &Environment) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut result = false;
    for rule in rules {
        if rule.applies(env) {
            result = rule.action == RuleAction::Allow;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{Arch, OsName};

    #[test]
    fn empty_rules_allow() {
        assert!(allowed(&[], &Environment::fixed(OsName::Linux, Arch::X86_64)));
    }

    #[test]
    fn os_specific_rules() {
        let json = r#"[
            {"action":"allow"},
            {"action":"disallow","os":{"name":"osx"}}
        ]"#;
        let rules: Vec<Rule> = serde_json::from_str(json).unwrap();
        assert!(allowed(&rules, &Environment::fixed(OsName::Linux, Arch::X86_64)));
        assert!(!allowed(&rules, &Environment::fixed(OsName::Osx, Arch::Arm64)));
    }

    #[test]
    fn only_allow_windows() {
        let json = r#"[{"action":"allow","os":{"name":"windows"}}]"#;
        let rules: Vec<Rule> = serde_json::from_str(json).unwrap();
        assert!(allowed(&rules, &Environment::fixed(OsName::Windows, Arch::X86_64)));
        assert!(!allowed(&rules, &Environment::fixed(OsName::Linux, Arch::X86_64)));
    }

    #[test]
    fn features_are_off_by_default() {
        let json = r#"[{"action":"allow","features":{"has_custom_resolution":true}}]"#;
        let rules: Vec<Rule> = serde_json::from_str(json).unwrap();
        let env = Environment::fixed(OsName::Linux, Arch::X86_64);
        assert!(!allowed(&rules, &env));
        assert!(allowed(
            &rules,
            &env.clone().with_feature("has_custom_resolution", true)
        ));
    }

    #[test]
    fn os_version_rules_use_the_system_version() {
        let json = r#"[{"action":"allow","os":{"version":"^6\\."}}]"#;
        let rules: Vec<Rule> = serde_json::from_str(json).unwrap();
        let env = Environment::fixed(OsName::Linux, Arch::X86_64);
        assert!(allowed(&rules, &env.clone().with_os_version("6.8.0-arch1")));
        assert!(!allowed(&rules, &env.with_os_version("10.0")));
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        assert!(Environment::current().os_version.is_some());
    }
}
