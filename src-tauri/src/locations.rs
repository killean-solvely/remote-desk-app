use crate::model::{download_target, Job};
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LocationAction {
    Reveal,
    Folder,
}

pub fn file_manager_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Finder"
    } else {
        "Explorer"
    }
}

fn unchanged(metadata: &std::fs::Metadata) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(
                "The saved location was redirected. Open its original location manually.".into(),
            );
        }
    }
    if metadata.file_type().is_symlink() {
        return Err(
            "The saved location was redirected. Open its original location manually.".into(),
        );
    }
    Ok(())
}

fn container(job: &Job) -> Result<PathBuf, String> {
    let path = Path::new(&job.destination);
    let expected = format!("RemoteDesk-{}", job.id);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || path.file_name().and_then(|n| n.to_str()) != Some(expected.as_str())
    {
        return Err("The saved download folder is invalid.".into());
    }
    // Inspect only the directory chain, never recursively walk downloaded contents.
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|error| {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                "Access to the download folder was denied. Check its permissions and try again."
            } else {
                "The download folder is unavailable. It may have been moved or deleted, or its drive may be disconnected."
            }
        })?;
        unchanged(&metadata)?;
        if !metadata.is_dir() {
            return Err("The saved download folder is no longer a folder.".into());
        }
    }
    std::fs::canonicalize(path).map_err(|_| "The download folder is unavailable. Try again.".into())
}

pub fn resolve(job: &Job, action: LocationAction) -> Result<PathBuf, String> {
    if action == LocationAction::Reveal && job.state != "completed" {
        return Err("Only completed downloads can be revealed. Use Open download folder to inspect partial files.".into());
    }
    let directory = container(job)?;
    if action == LocationAction::Folder {
        return Ok(directory);
    }
    let target = download_target(&directory, &job.path)?;
    let metadata = std::fs::symlink_metadata(&target).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            "Access to the downloaded item was denied. Check its permissions and try again."
        } else {
            "The downloaded item is no longer at its saved location. Use Open download folder to inspect what remains."
        }
    })?;
    unchanged(&metadata)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err("The downloaded item is no longer a regular file or folder.".into());
    }
    let target = std::fs::canonicalize(target)
        .map_err(|_| "The downloaded item is unavailable. Try again.")?;
    if target.parent() != Some(directory.as_path()) {
        return Err("The downloaded item is outside its saved download folder.".into());
    }
    Ok(target)
}

// The webview supplies a job ID and enum only. Neither opener is exposed as a
// generic IPC capability, and open_path is restricted to the known job container.
pub fn open(job: &Job, action: LocationAction) -> Result<(), String> {
    dispatch(job, action, |path, action| {
        let path = shell_path(path)?;
        match action {
            LocationAction::Reveal => tauri_plugin_opener::reveal_item_in_dir(&path),
            LocationAction::Folder => tauri_plugin_opener::open_path(&path, None::<&str>),
        }
        .map_err(|_| {
            format!(
                "Could not open {}. Check that the saved location is available and try again.",
                file_manager_name()
            )
        })
    })
}

fn shell_path(path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        // Explorer rejects some extended-length paths. Convert only when the
        // ordinary shell spelling resolves to exactly the already checked item.
        // Keep canonical extended paths for every filesystem safety check.
        let text = path
            .to_str()
            .ok_or("The saved location cannot be displayed")?;
        let candidate = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{rest}"))
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            PathBuf::from(rest)
        } else {
            path.to_path_buf()
        };
        if std::fs::canonicalize(&candidate).ok().as_deref() != Some(path) {
            return Err("The saved location changed or cannot be opened in Explorer.".into());
        }
        Ok(candidate)
    }
    #[cfg(not(windows))]
    Ok(path.to_path_buf())
}

fn dispatch<F>(job: &Job, action: LocationAction, opener: F) -> Result<(), String>
where
    F: FnOnce(&Path, LocationAction) -> Result<(), String>,
{
    let path = resolve(job, action)?;
    opener(&path, action)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        job: Job,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("remote-desk-locations-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let root = std::fs::canonicalize(root).unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let directory = root.join(format!("RemoteDesk-{id}"));
            std::fs::create_dir(&directory).unwrap();
            let job = Job {
                id,
                remote_id: "deleted-connection".into(),
                path: "Projects/Artist's sample, report.zip".into(),
                destination: directory.to_string_lossy().into_owned(),
                state: "completed".into(),
                detail: String::new(),
                stats: serde_json::json!({}),
            };
            Self { root, job }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn reveals_exact_files_and_folders_without_executing_them() {
        let mut fixture = Fixture::new();
        for name in [
            "Artist's sample, report.zip",
            "caf\u{e9}.exe",
            "Example.app",
            "Empty folder",
        ] {
            fixture.job.path = format!("Remote:parent/{name}");
            let target =
                download_target(Path::new(&fixture.job.destination), &fixture.job.path).unwrap();
            if name.ends_with(".app") || name == "Empty folder" {
                std::fs::create_dir(&target).unwrap();
            } else {
                std::fs::write(&target, b"local fixture, never executable").unwrap();
            }
            dispatch(&fixture.job, LocationAction::Reveal, |path, action| {
                assert_eq!(path, std::fs::canonicalize(&target).unwrap());
                assert_eq!(action, LocationAction::Reveal);
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(
            resolve(&fixture.job, LocationAction::Folder).unwrap(),
            PathBuf::from(&fixture.job.destination)
        );
    }
    #[test]
    fn partial_jobs_only_open_their_container_and_missing_items_do_not_mutate_history() {
        let mut fixture = Fixture::new();
        for state in [
            "queued",
            "preparing",
            "running",
            "failed",
            "canceled",
            "interrupted",
        ] {
            fixture.job.state = state.into();
            assert!(resolve(&fixture.job, LocationAction::Reveal).is_err());
            assert!(resolve(&fixture.job, LocationAction::Folder).is_ok());
        }
        fixture.job.state = "completed".into();
        assert!(resolve(&fixture.job, LocationAction::Reveal)
            .unwrap_err()
            .contains("no longer"));
        assert_eq!(fixture.job.state, "completed");
        assert!(resolve(&fixture.job, LocationAction::Folder).is_ok());
        std::fs::remove_dir(&fixture.job.destination).unwrap();
        assert!(resolve(&fixture.job, LocationAction::Folder)
            .unwrap_err()
            .contains("unavailable"));
        assert!(!Path::new(&fixture.job.destination).exists());
    }
    #[test]
    fn rejects_invalid_targets_containers_and_actions() {
        let mut fixture = Fixture::new();
        for path in [
            "../escape",
            "/absolute",
            "parent/../escape",
            "parent/CON.txt",
            "parent/x:stream",
        ] {
            fixture.job.path = path.into();
            assert!(resolve(&fixture.job, LocationAction::Reveal).is_err());
        }
        fixture.job.destination = fixture.root.to_string_lossy().into_owned();
        assert!(resolve(&fixture.job, LocationAction::Folder).is_err());
        assert!(serde_json::from_str::<LocationAction>("\"execute\"").is_err());
    }
    fn link_directory(real: &Path, link: &Path) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Junctions exercise the same reparse guard without requiring the
            // developer-mode/admin privilege needed by Windows symlinks.
            let real = real.to_string_lossy();
            let link = link.to_string_lossy();
            let real = real.strip_prefix(r"\\?\").unwrap_or(&real);
            let link = link.strip_prefix(r"\\?\").unwrap_or(&link);
            let result = std::process::Command::new("cmd.exe")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(real)
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "Could not create junction fixture: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(real, link).unwrap();
    }
    #[test]
    fn long_native_paths_and_opener_failures_preserve_job_state() {
        let mut fixture = Fixture::new();
        let deep = fixture.root.join("long directory name ".repeat(10));
        std::fs::create_dir(&deep).unwrap();
        let directory = deep.join(format!("RemoteDesk-{}", fixture.job.id));
        std::fs::create_dir(&directory).unwrap();
        fixture.job.destination = directory.to_string_lossy().into_owned();
        let target = download_target(&directory, &fixture.job.path).unwrap();
        std::fs::write(&target, b"generated fixture").unwrap();
        assert_eq!(
            resolve(&fixture.job, LocationAction::Reveal).unwrap(),
            std::fs::canonicalize(&target).unwrap()
        );
        let canonical = std::fs::canonicalize(&target).unwrap();
        let shell = shell_path(&canonical).unwrap();
        assert_eq!(std::fs::canonicalize(&shell).unwrap(), canonical);
        #[cfg(windows)]
        assert!(!shell.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(
            dispatch(&fixture.job, LocationAction::Reveal, |_, _| Err(
                "File manager unavailable".into()
            ))
            .unwrap_err(),
            "File manager unavailable"
        );
        assert_eq!(fixture.job.state, "completed");
        assert_eq!(std::fs::read(&target).unwrap(), b"generated fixture");
    }
    #[test]
    fn rejects_redirected_container_and_item() {
        let fixture = Fixture::new();
        let real = fixture.root.join("real");
        std::fs::create_dir(&real).unwrap();
        let target =
            download_target(Path::new(&fixture.job.destination), &fixture.job.path).unwrap();
        link_directory(&real, &target);
        assert!(resolve(&fixture.job, LocationAction::Reveal)
            .unwrap_err()
            .contains("redirected"));
        #[cfg(windows)]
        std::fs::remove_dir(&target).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&target).unwrap();
        std::fs::remove_dir(&fixture.job.destination).unwrap();
        link_directory(&real, Path::new(&fixture.job.destination));
        assert!(resolve(&fixture.job, LocationAction::Folder)
            .unwrap_err()
            .contains("redirected"));
        #[cfg(windows)]
        std::fs::remove_dir(&fixture.job.destination).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&fixture.job.destination).unwrap();
    }
}
