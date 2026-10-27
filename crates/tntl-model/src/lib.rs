//! Minecraft version metadata, rules and launch arguments.
//!
//! This crate is pure data: it performs no I/O and knows nothing about the
//! filesystem or the network.

#![forbid(unsafe_code)]

mod asset;
mod env;
mod library;
mod maven;
mod rules;
mod version;

pub use asset::{AssetIndex, AssetObject, AssetRef};
pub use env::{Arch, Environment, OsName};
pub use library::{Artifact, Extract, Library, LibraryDownloads, MavenName, NATIVE_ARCH_MARKER, classifier_for};
pub use maven::maven_path;
pub use rules::{Rule, RuleAction, allowed};
pub use version::{
    Argument, Arguments, JavaVersion, Logging, LoggingFile, PatchRef, VersionDownloads, VersionJson, VersionManifest,
    VersionSummary,
};

/// Replace `${key}` placeholders using `lookup`. Unknown placeholders are kept
/// verbatim so a broken plan is visible instead of silently mangled.
pub fn expand_placeholders(arg: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(start) = rest.find("${") {
        let Some(end_rel) = rest[start + 2..].find('}') else {
            break;
        };
        let end = start + 2 + end_rel;
        out.push_str(&rest[..start]);
        let key = &rest[start + 2..end];
        match lookup(key) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[start..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::expand_placeholders;

    #[test]
    fn expands_known_and_keeps_unknown() {
        let lookup = |key: &str| match key {
            "a" => Some("one".to_string()),
            "b" => Some("two".to_string()),
            _ => None,
        };
        assert_eq!(expand_placeholders("${a}-${b}", &lookup), "one-two");
        assert_eq!(expand_placeholders("${a}-${c}", &lookup), "one-${c}");
        assert_eq!(expand_placeholders("plain", &lookup), "plain");
    }
}
