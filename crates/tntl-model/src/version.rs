use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::asset::AssetRef;
use crate::env::Environment;
use crate::library::{Artifact, Library};
use crate::rules::allowed;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrVec {
    #[default]
    Empty,
    One(String),
    Many(Vec<String>),
}

impl StringOrVec {
    fn flatten(&self, out: &mut Vec<String>) {
        match self {
            StringOrVec::Empty => {}
            StringOrVec::One(value) => out.push(value.clone()),
            StringOrVec::Many(values) => out.extend(values.iter().cloned()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    Plain(String),
    Conditional {
        rules: Vec<crate::Rule>,
        value: StringOrVec,
    },
}

impl Argument {
    pub fn collect(&self, env: &Environment, out: &mut Vec<String>) {
        match self {
            Argument::Plain(value) => out.push(value.clone()),
            Argument::Conditional { rules, value } => {
                if allowed(rules, env) {
                    value.flatten(out);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<Argument>,
    #[serde(default)]
    pub jvm: Vec<Argument>,
}

impl Arguments {
    pub fn game_args(&self, env: &Environment) -> Vec<String> {
        let mut out = Vec::new();
        for arg in &self.game {
            arg.collect(env, &mut out);
        }
        out
    }

    pub fn jvm_args(&self, env: &Environment) -> Vec<String> {
        let mut out = Vec::new();
        for arg in &self.jvm {
            arg.collect(env, &mut out);
        }
        out
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub major_version: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoggingFile {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Logging {
    #[serde(default)]
    pub argument: Option<String>,
    #[serde(default)]
    pub file: Option<LoggingFile>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionDownloads {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_mappings: Option<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_mappings: Option<Artifact>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    #[serde(rename = "type", default)]
    pub version_type: Option<String>,
    #[serde(default)]
    pub inherits_from: Option<String>,
    #[serde(default)]
    pub main_class: Option<String>,
    /// Pre-1.13 argument format.
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub asset_index: Option<AssetRef>,
    #[serde(default)]
    pub assets: Option<String>,
    #[serde(default)]
    pub downloads: Option<VersionDownloads>,
    #[serde(default)]
    pub java_version: Option<JavaVersion>,
    #[serde(default)]
    pub logging: Option<BTreeMap<String, Logging>>,
    #[serde(default)]
    pub compliance_level: Option<u32>,
    #[serde(default)]
    pub release_time: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
    /// PCL2 and HMCL composite profiles (`root: true`) keep the game build and
    /// the loader as patch entries; their own `id` is the pack version.
    #[serde(default)]
    pub patches: Vec<PatchRef>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchRef {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub version: String,
}

impl VersionJson {
    /// Merged game arguments for this single version (no inheritance).
    pub fn game_args(&self, env: &Environment) -> Vec<String> {
        if let Some(arguments) = &self.arguments
            && !arguments.game.is_empty()
        {
            return arguments.game_args(env);
        }
        self.minecraft_arguments
            .as_deref()
            .map(|legacy| legacy.split_whitespace().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    pub fn jvm_args(&self, env: &Environment) -> Vec<String> {
        self.arguments
            .as_ref()
            .map(|arguments| arguments.jvm_args(env))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionSummary {
    pub id: String,
    #[serde(rename = "type")]
    pub version_type: String,
    pub url: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(rename = "releaseTime", default)]
    pub release_time: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestLatest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionManifest {
    #[serde(default)]
    pub latest: ManifestLatest,
    pub versions: Vec<VersionSummary>,
}
