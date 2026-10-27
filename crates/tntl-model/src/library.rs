use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::maven_path;

/// Marker used by `natives` maps, e.g. `natives-windows-${arch}`.
pub const NATIVE_ARCH_MARKER: &str = "${arch}";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibraryDownloads {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifiers: Option<BTreeMap<String, Artifact>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Library {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<LibraryDownloads>,
    /// OS name -> natives classifier, e.g. `{"windows": "natives-windows-${arch}"}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub natives: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract: Option<Extract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<crate::Rule>>,
    /// Maven base URL for libraries without a `downloads` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clientreq: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serverreq: Option<bool>,
}

/// Parsed Maven coordinate: `group:artifact:version[:classifier][@ext]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MavenName {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub classifier: Option<String>,
    pub extension: String,
}

impl MavenName {
    pub fn parse(raw: &str) -> Option<Self> {
        let (coords, extension) = match raw.rsplit_once('@') {
            Some((coords, ext)) => (coords, ext.to_string()),
            None => (raw, "jar".to_string()),
        };
        let mut parts = coords.split(':');
        let group = parts.next()?.to_string();
        let artifact = parts.next()?.to_string();
        let version = parts.next()?.to_string();
        let classifier = parts.next().map(ToString::to_string);
        if group.is_empty() || artifact.is_empty() || version.is_empty() || parts.next().is_some() {
            return None;
        }
        Some(Self {
            group,
            artifact,
            version,
            classifier,
            extension,
        })
    }

    pub fn with_classifier(&self, classifier: &str) -> Self {
        let mut next = self.clone();
        next.classifier = Some(classifier.to_string());
        next
    }

    pub fn path(&self) -> String {
        let file = match &self.classifier {
            Some(classifier) => format!("{}-{}-{}.{}", self.artifact, self.version, classifier, self.extension),
            None => format!("{}-{}.{}", self.artifact, self.version, self.extension),
        };
        maven_path(&self.group, &self.artifact, &self.version, &file)
    }
}

/// Classifiers may carry a `${arch}` placeholder.
pub fn classifier_for(env_os: &str, env_bits: &str, library: &Library) -> Option<String> {
    let natives = library.natives.as_ref()?;
    let raw = natives.get(env_os)?;
    Some(raw.replace(NATIVE_ARCH_MARKER, env_bits))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_maven_names() {
        let name = MavenName::parse("org.lwjgl:lwjgl:3.3.1:natives-linux").unwrap();
        assert_eq!(name.group, "org.lwjgl");
        assert_eq!(name.extension, "jar");
        assert_eq!(name.path(), "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-linux.jar");

        let name = MavenName::parse("com.example:thing:1.0@zip").unwrap();
        assert_eq!(name.extension, "zip");
        assert_eq!(name.path(), "com/example/thing/1.0/thing-1.0.zip");
        assert!(MavenName::parse("broken").is_none());
    }

    #[test]
    fn arch_placeholder_is_replaced() {
        let lib: Library =
            serde_json::from_str(r#"{"name":"a:b:1","natives":{"windows":"natives-windows-${arch}"}}"#).unwrap();
        assert_eq!(
            classifier_for("windows", "64", &lib).as_deref(),
            Some("natives-windows-64")
        );
        assert_eq!(classifier_for("linux", "64", &lib), None);
    }
}
