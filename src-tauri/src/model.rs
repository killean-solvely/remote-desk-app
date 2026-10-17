use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remote {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub root: String,
    pub auth: String,
    pub known_hosts: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub rclone: String,
    pub transfers: u8,
    pub streams: u8,
    pub cutoff: String,
    pub chunk: String,
    pub bandwidth: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            rclone: String::new(),
            transfers: 4,
            streams: 16,
            cutoff: "100M".into(),
            chunk: "16M".into(),
            bandwidth: String::new(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Entry {
    pub path: String,
    pub name: String,
    pub size: i64,
    pub is_dir: bool,
    #[serde(default)]
    pub mod_time: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub remote_id: String,
    pub path: String,
    pub destination: String,
    pub state: String,
    pub detail: String,
    pub stats: Value,
}
// Remote SFTP components are not local Windows/macOS filenames. Preserve their
// spelling while rejecting traversal and protocol separators only.
pub fn remote_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err("Invalid remote path component".into());
    }
    Ok(())
}
pub fn remote_relative(path: &str) -> Result<String, String> {
    if path.is_empty() {
        return Ok(String::new());
    }
    if path.starts_with('/') {
        return Err("Use a relative remote path".into());
    }
    for part in path.split('/') {
        remote_name(part)?;
    }
    Ok(path.into())
}
pub fn download_name(path: &str) -> Result<&str, String> {
    remote_relative(path)?;
    let name = path
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .ok_or("Select a file or folder")?;
    safe_name(name).map_err(|_| format!("Cannot download {name:?}: its name cannot be safely saved on Windows and macOS. Choose another item or rename it on the server."))?;
    Ok(name)
}
pub fn download_target(
    destination: &std::path::Path,
    path: &str,
) -> Result<std::path::PathBuf, String> {
    Ok(destination.join(download_name(path)?))
}
pub fn relative(path: &str) -> Result<String, String> {
    if path.is_empty() {
        return Ok(String::new());
    }
    remote_relative(path)?;
    for part in path.split('/') {
        safe_name(part).map_err(|_| format!("Cannot download {path:?}: its name cannot be safely saved on Windows and macOS. Choose another item or rename it on the server."))?;
    }
    Ok(path.into())
}
pub fn safe_name(name: &str) -> Result<(), String> {
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || [
            "CON", "PRN", "AUX", "NUL", "COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³",
        ]
        .contains(&base.as_str())
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        return Err("A remote filename cannot be safely represented on both platforms".into());
    }
    Ok(())
}
pub fn source(r: &Remote, path: &str) -> Result<String, String> {
    let p = remote_relative(path)?;
    Ok(format!(
        "app:{}{}{}",
        r.root,
        if r.root.is_empty() || r.root.ends_with('/') || p.is_empty() {
            ""
        } else {
            "/"
        },
        p
    ))
}
pub fn progress(line: &str) -> Option<Value> {
    serde_json::from_str::<Value>(line)
        .ok()?
        .get("stats")
        .filter(|v| v.is_object())
        .cloned()
}
pub fn retryable(state: &str) -> bool {
    matches!(state, "failed" | "canceled" | "interrupted")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traversal_and_portability() {
        for p in [
            "../escape",
            "/absolute",
            "a/../b",
            "a\\b",
            "a//b",
            "CON.txt",
            "a:stream",
            "trailing.",
            "LPT1",
        ] {
            assert!(relative(p).is_err(), "{p}");
        }
        assert!(relative("Projects/Artist's Reference [Sample]").is_ok());
    }
    #[test]
    fn remote_paths_preserve_names_that_cannot_be_saved_locally() {
        for path in [
            "Parent:folder/file.zip",
            "CON.txt",
            "trailing.",
            "name?",
            "a\\b",
            "a\nb",
        ] {
            assert!(remote_relative(path).is_ok(), "{path:?}");
        }
        for path in ["../escape", "/absolute", "a/../b", "a//b", "a/./b", "a\0b"] {
            assert!(remote_relative(path).is_err(), "{path:?}");
        }
        assert_eq!(download_name("Parent:folder/file.zip").unwrap(), "file.zip");
        assert!(download_name("Parent/CON.txt")
            .unwrap_err()
            .contains("CON.txt"));
        assert!(relative("Parent:folder/file.zip").is_err());
    }
    #[test]
    fn parse_json_stats() {
        assert_eq!(
            progress(r#"{"stats":{"bytes":42,"transferring":[{"name":"x","bytes":2}]}}"#).unwrap()
                ["bytes"],
            42
        );
        assert!(progress("Transferred: 42").is_none());
        assert!(progress("{}").is_none());
    }
    #[test]
    fn states() {
        assert!(retryable("interrupted"));
        assert!(!retryable("running"));
        assert!(!retryable("completed"));
    }
    #[test]
    fn root_join() {
        let r = Remote {
            id: "x".into(),
            label: "x".into(),
            host: "h".into(),
            port: 22,
            user: "u".into(),
            root: "/data".into(),
            auth: "agent".into(),
            known_hosts: "key".into(),
        };
        assert_eq!(source(&r, "a/b").unwrap(), "app:/data/a/b");
        assert!(source(&r, "../data2").is_err());
    }
}
