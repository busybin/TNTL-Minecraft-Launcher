use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRef {
    pub id: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub total_size: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndex {
    #[serde(default)]
    pub objects: BTreeMap<String, AssetObject>,
    /// Old indexes spell this in snake case, unlike the rest of the file.
    #[serde(default, rename = "map_to_resources")]
    pub map_to_resources: Option<bool>,
    #[serde(default)]
    pub r#virtual: Option<bool>,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_legacy_index_flags() {
        let index: AssetIndex = serde_json::from_str(r#"{"map_to_resources":true,"objects":{}}"#).unwrap();
        assert_eq!(index.map_to_resources, Some(true));
        assert_eq!(index.r#virtual, None);
        let index: AssetIndex = serde_json::from_str(r#"{"virtual":true,"objects":{}}"#).unwrap();
        assert_eq!(index.r#virtual, Some(true));
        assert_eq!(index.map_to_resources, None);
    }
}
