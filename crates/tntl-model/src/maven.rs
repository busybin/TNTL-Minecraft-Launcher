/// Build a Maven repository path: `group/artifact/version/file`.
pub fn maven_path(group: &str, artifact: &str, version: &str, file: &str) -> String {
    format!("{}/{}/{}/{}", group.replace('.', "/"), artifact, version, file)
}
