use crate::model::*;
use serde_json::Value;
use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use unicode_normalization::UnicodeNormalization;

pub fn process(executable: &str) -> Command {
    let mut c = Command::new(executable);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("RCLONE_") {
            c.env_remove(k);
        }
    }
    c.args(["--config", if cfg!(windows) { "NUL" } else { "/dev/null" }]);
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    c
}
pub fn validate_binary(path: &str) -> Result<String, String> {
    if !Path::new(path).is_absolute() || !Path::new(path).is_file() {
        return Err("The transfer engine is missing. Reinstall Remote Desk to restore it".into());
    }
    let (v, _) = run(
        process(path).arg("version"),
        None,
        Duration::from_secs(15),
        |_| {},
    )?;
    let version = String::from_utf8(v).map_err(|_| "Invalid rclone version")?;
    if !version.starts_with("rclone v1.") {
        return Err("Expected rclone v1.x".into());
    }
    let (help, _) = run(
        process(path).args(["help", "flags"]),
        None,
        Duration::from_secs(15),
        |_| {},
    )?;
    let help = String::from_utf8_lossy(&help);
    for f in [
        "--multi-thread-streams",
        "--multi-thread-chunk-size",
        "--use-json-log",
        "--sftp-known-hosts-file",
        "--sftp-host-key-algorithms",
        "--sftp-skip-links",
        "--immutable",
    ] {
        if !help.contains(f) {
            return Err(format!("rclone is missing required option {f}"));
        }
    }
    Ok(version.lines().next().unwrap_or("").into())
}
// Limit negotiation to algorithms represented by trusted keys. The known_hosts
// callback still verifies the actual host/key; this never accepts a new key.
pub fn pinned_algorithms(contents: &str) -> Result<String, String> {
    let mut algorithms: Vec<&str> = Vec::new();
    for line in contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first() == Some(&"@revoked") {
            continue;
        }
        let candidates: Vec<&str> = if fields.first() == Some(&"@cert-authority") {
            vec![
                "ssh-ed25519-cert-v01@openssh.com",
                "ecdsa-sha2-nistp256-cert-v01@openssh.com",
                "ecdsa-sha2-nistp384-cert-v01@openssh.com",
                "ecdsa-sha2-nistp521-cert-v01@openssh.com",
                "rsa-sha2-512-cert-v01@openssh.com",
                "rsa-sha2-256-cert-v01@openssh.com",
            ]
        } else {
            match fields.get(1).copied() {
                Some("ssh-rsa") => vec!["rsa-sha2-512", "rsa-sha2-256"],
                Some(
                    a @ ("ssh-ed25519"
                    | "ecdsa-sha2-nistp256"
                    | "ecdsa-sha2-nistp384"
                    | "ecdsa-sha2-nistp521"),
                ) => vec![a],
                _ => vec![],
            }
        };
        for algorithm in candidates {
            if !algorithms.contains(&algorithm) {
                algorithms.push(algorithm);
            }
        }
    }
    if algorithms.is_empty() {
        return Err("The pinned known_hosts file contains no supported host-key algorithms".into());
    }
    Ok(algorithms.join(" "))
}
pub fn remote_command(s: &Settings, r: &Remote, hosts: &Path) -> Result<Command, String> {
    let mut c = process(&s.rclone);
    let trusted_keys =
        std::fs::read_to_string(hosts).map_err(|_| "Cannot read pinned known_hosts file")?;
    c.env(
        "RCLONE_CONFIG_APP_HOST_KEY_ALGORITHMS",
        pinned_algorithms(&trusted_keys)?,
    );
    c.args([
        "--use-json-log",
        "--contimeout",
        "15s",
        "--timeout",
        "60s",
        "--retries",
        "3",
        "--retries-sleep",
        "5s",
    ]);
    for (k, v) in [
        ("TYPE", "sftp"),
        ("HOST", r.host.as_str()),
        ("USER", r.user.as_str()),
        ("PORT", &r.port.to_string()),
        (
            "KNOWN_HOSTS_FILE",
            hosts.to_str().ok_or("Unsupported host key path")?,
        ),
        ("SHELL_TYPE", "none"),
        ("DISABLE_HASHCHECK", "true"),
        ("SKIP_LINKS", "true"),
    ] {
        c.env(format!("RCLONE_CONFIG_APP_{k}"), v);
    }
    match r.auth.as_str() {
        "agent" => {
            c.env("RCLONE_CONFIG_APP_KEY_USE_AGENT", "true");
        }
        "password" => {
            let mut secret = keyring::Entry::new("com.remotedesk.app", &r.id)
                .map_err(|_| "Cannot open OS credential store")?
                .get_password()
                .map_err(|_| {
                    "Password missing. Open Settings and choose Set password for this connection"
                })?;
            c.env("RCLONE_CONFIG_APP_PASS", &secret);
            use zeroize::Zeroize;
            secret.zeroize();
        }
        _ => return Err("Unsupported authentication mode".into()),
    };
    Ok(c)
}
// copyto rejects directory-only flags and does not enforce --immutable itself.
// Check existing file sizes before starting, then skip existing files on retry.
pub fn transfer_options(command: &mut Command, settings: &Settings, is_dir: bool) {
    command.args([
        "--transfers",
        &settings.transfers.to_string(),
        "--multi-thread-streams",
        &settings.streams.to_string(),
        "--multi-thread-cutoff",
        &settings.cutoff,
        "--multi-thread-chunk-size",
        &settings.chunk,
        "--stats",
        "1s",
        "--stats-log-level",
        "NOTICE",
        "--immutable",
    ]);
    if is_dir {
        command.arg("--create-empty-src-dirs");
    } else {
        command.arg("--ignore-existing");
    }
    if !settings.bandwidth.is_empty() {
        command.args(["--bwlimit", &settings.bandwidth]);
    }
}
// Bounded stdout; stderr never reaches disk or the webview. Only structured stats do.
pub fn diagnostic(text: &str) -> Option<&'static str> {
    let text = text.to_lowercase();
    if text.contains("unknown flag:") {
        Some("The transfer engine rejected an application option. Update Remote Desk and its rclone engine.")
    } else if text.contains("knownhosts") || text.contains("host key") {
        Some("SSH host-key verification failed. Independently verify the server key; add a newly verified remote for intentional rotation.")
    } else if text.contains("unable to authenticate")
        || text.contains("no supported methods remain")
        || text.contains("ssh agent")
        || text.contains("ssh-agent")
    {
        Some("SSH authentication failed. Check the saved username and update its password in Settings, or check your SSH agent.")
    } else if text.contains("immutable file modified") {
        Some("An existing destination file differs. It was protected from overwrite; inspect the isolated job directory.")
    } else if text.contains("no space left") || text.contains("disk full") {
        Some("The destination disk is full.")
    } else if text.contains("permission denied") {
        Some("Access was denied by the server or local filesystem.")
    } else if text.contains("connection refused")
        || text.contains("no such host")
        || text.contains("i/o timeout")
    {
        Some("Could not reach the SFTP server. Check the host, SSH port and network.")
    } else {
        None
    }
}
pub fn run<F: FnMut(Value)>(
    command: &mut Command,
    cancel: Option<Arc<AtomicBool>>,
    timeout: Duration,
    mut on_stats: F,
) -> Result<(Vec<u8>, bool), String> {
    if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
        return Err("Canceled".into());
    }
    let mut child = command.spawn().map_err(|_| {
        "Could not start the transfer engine. Check that Remote Desk is installed completely"
    })?;
    let stdout = child.stdout.take().ok_or("Missing stdout")?;
    let stderr = child.stderr.take().ok_or("Missing stderr")?;
    let (tx, rx) = std::sync::mpsc::sync_channel(32);
    let out = thread::spawn(move || {
        let mut data = Vec::new();
        let result = stdout.take(32 * 1024 * 1024 + 1).read_to_end(&mut data);
        (result, data)
    });
    let err = thread::spawn(move || {
        let mut reader = stderr;
        let mut buf = [0u8; 4096];
        let mut line = Vec::new();
        let mut dropping = false;
        let mut had_error = false;
        let mut reason = None;
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                if b == b'\n' {
                    if !dropping {
                        let text = String::from_utf8_lossy(&line);
                        if let Some(classified) = diagnostic(&text) {
                            reason = Some(classified);
                        }
                        if let Some(s) = progress(&text) {
                            let _ = tx.try_send(s);
                        }
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            had_error |= v["level"] == "error";
                        }
                    }
                    line.clear();
                    dropping = false;
                } else if line.len() < 65536 {
                    line.push(b)
                } else {
                    dropping = true;
                }
            }
        }
        (had_error, reason)
    });
    let start = Instant::now();
    let mut aborted = None;
    let status = loop {
        while let Ok(s) = rx.try_recv() {
            on_stats(s)
        }
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
            aborted = Some("Canceled");
            let _ = child.kill();
        }
        if start.elapsed() > timeout {
            aborted = Some("Operation timed out");
            let _ = child.kill();
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Cannot monitor rclone".into());
            }
        }
    };
    let _ = child.wait();
    let (read, data) = out.join().map_err(|_| "Output reader failed")?;
    let (had_error, reason) = err.join().map_err(|_| "Log reader failed")?;
    while let Ok(s) = rx.try_recv() {
        on_stats(s)
    }
    if let Some(e) = aborted {
        return Err(e.into());
    }
    if data.len() > 32 * 1024 * 1024 {
        return Err("Directory exceeds the 32 MiB listing limit; browse a smaller subtree".into());
    }
    read.map_err(|_| "Cannot read rclone output")?;
    if !status.success() {
        if let Some(reason) = reason {
            return Err(reason.into());
        }
        return Err("rclone failed: check connectivity, credentials, pinned host key, source paths, and local space. Raw process logs are withheld to protect secrets.".into());
    }
    Ok((data, had_error))
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct FolderSize {
    pub bytes: u64,
    pub count: u64,
}
fn parse_folder_size(data: &[u8]) -> Result<FolderSize, String> {
    #[derive(serde::Deserialize)]
    struct Report {
        bytes: u64,
        count: u64,
        #[serde(default)]
        sizeless: u64,
    }
    let report: Report = serde_json::from_slice(data).map_err(|_| "Invalid folder size report")?;
    if report.sizeless != 0 {
        return Err("Some files did not report a size. A complete total is unavailable.".into());
    }
    Ok(FolderSize {
        bytes: report.bytes,
        count: report.count,
    })
}
// Metadata only: no copy, hashing, or shell access. The shared SFTP configuration
// pins host keys, uses the OS credential store and excludes symlinks.
pub fn folder_size(
    s: &Settings,
    r: &Remote,
    hosts: &Path,
    path: &str,
    cancel: Arc<AtomicBool>,
) -> Result<FolderSize, String> {
    let mut c = remote_command(s, r, hosts)?;
    c.args(["size", &source(r, path)?, "--json"]);
    read_folder_size(&mut c, cancel)
}
fn read_folder_size(c: &mut Command, cancel: Arc<AtomicBool>) -> Result<FolderSize, String> {
    let (data, had_error) = run(c, Some(cancel), Duration::from_secs(1800), |_| {})?;
    if had_error {
        return Err("Some folders could not be read. A complete size is unavailable.".into());
    }
    parse_folder_size(&data)
}

pub fn list(
    s: &Settings,
    r: &Remote,
    hosts: &Path,
    path: &str,
    recursive: bool,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Vec<Entry>, String> {
    let mut c = remote_command(s, r, hosts)?;
    c.args(["lsjson", &source(r, path)?, "--no-mimetype"]);
    if recursive {
        c.arg("--recursive");
    }
    let (data, _) = run(&mut c, cancel, Duration::from_secs(300), |_| {})?;
    let entries: Vec<Entry> =
        serde_json::from_slice(&data).map_err(|_| "Invalid rclone lsjson output")?;
    validate_listing(&entries)?;
    Ok(entries)
}
// Remote names are case-sensitive. Browsing must preserve both spellings; each
// selected item receives a separate job directory when it is downloaded.
pub fn validate_listing(entries: &[Entry]) -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    for e in entries {
        relative(&e.path)?;
        safe_name(&e.name)?;
        if e.path.is_empty() {
            return Err("Invalid empty listing path".into());
        }
        if !names.insert(&e.path) {
            return Err(
                "The remote listing contains a duplicate path; refresh and try again".into(),
            );
        }
    }
    Ok(())
}
// A folder copy shares one destination tree, unlike browsing or separate jobs.
// Check implicit parent paths too, so differently cased directories cannot merge.
pub fn validate_manifest(entries: &[Entry]) -> Result<(), String> {
    validate_listing(entries)?;
    let mut names = std::collections::HashMap::new();
    for entry in entries {
        let mut prefix = String::new();
        for component in entry.path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let key = prefix.nfc().collect::<String>().to_lowercase();
            if let Some(previous) = names.insert(key, prefix.clone()) {
                if previous != prefix {
                    return Err(format!(
                        "Cannot download this folder: {previous:?} and {prefix:?} would share the same local path. Download these items separately, or rename one on the server."
                    ));
                }
            }
        }
    }
    Ok(())
}
pub fn check_existing_file(path: &Path, expected_size: u64) -> Result<(), String> {
    clean_tree(path)?;
    let metadata = std::fs::metadata(path).map_err(|_| "Cannot inspect destination file")?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err("An existing destination file differs. It was protected from overwrite; inspect the isolated job directory.".into());
    }
    Ok(())
}
pub fn clean_tree(path: &Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(path).map_err(|_| "Cannot inspect destination")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err("Destination contains a reparse point".into());
        }
    }
    if m.file_type().is_symlink() {
        return Err("Destination contains a symlink".into());
    }
    if m.is_dir() {
        for item in std::fs::read_dir(path).map_err(|_| "Cannot inspect destination")? {
            clean_tree(&item.map_err(|_| "Cannot inspect destination")?.path())?;
        }
    }
    Ok(())
}
pub fn clean_ancestors(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Destination must be an absolute local path".into());
    }
    for ancestor in path.ancestors() {
        let m = std::fs::symlink_metadata(ancestor)
            .map_err(|_| "Cannot inspect destination ancestors")?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & 0x400 != 0 {
                return Err("Destination ancestor is a reparse point".into());
            }
        }
        if m.file_type().is_symlink() || !m.is_dir() {
            return Err("Destination ancestors must be real directories".into());
        }
    }
    Ok(())
}
#[allow(dead_code)] // Used by the separate native terminal helper, never exposed over IPC.
pub fn provision(id: &str, executable: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(id).map_err(|_| "Expected the remote ID shown in Settings")?;
    validate_binary(executable)?;
    let secret = zeroize::Zeroizing::new(
        rpassword::prompt_password("SFTP password (stored in OS keychain): ")
            .map_err(|_| "Cannot read password")?,
    );
    store_password(id, executable, secret)
}
pub fn store_password(
    id: &str,
    executable: &str,
    secret: zeroize::Zeroizing<String>,
) -> Result<(), String> {
    if secret.is_empty() || secret.contains(['\r', '\n', '\0']) {
        return Err("Password must be nonempty and one line".into());
    }
    let mut c = process(executable);
    c.args(["obscure", "-"]).stdin(Stdio::piped());
    let mut child = c.spawn().map_err(|_| "Cannot run rclone obscure")?;
    let result = child
        .stdin
        .take()
        .ok_or("Missing input")?
        .write_all(secret.as_bytes());
    drop(secret);
    result.map_err(|_| "Cannot pass password")?;
    let output = child
        .wait_with_output()
        .map_err(|_| "Cannot obscure password")?;
    let success = output.status.success();
    let value = zeroize::Zeroizing::new(output.stdout);
    if !success {
        return Err("Cannot obscure password".into());
    }
    let value = std::str::from_utf8(&value).map_err(|_| "Invalid credential encoding")?;
    keyring::Entry::new("com.remotedesk.app", id)
        .map_err(|_| "Credential store unavailable")?
        .set_password(value.trim())
        .map_err(|_| "OS credential store refused password")?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn size_cancel_fixture() {
        if std::env::var_os("RDESK_SIZE_CANCEL_FIXTURE").is_some() {
            thread::sleep(Duration::from_secs(30));
        }
    }
    #[test]
    fn folder_size_cancels_a_running_process() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "engine::tests::size_cancel_fixture",
                "--nocapture",
            ])
            .env("RDESK_SIZE_CANCEL_FIXTURE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let flag = Arc::new(AtomicBool::new(false));
        let signal = flag.clone();
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(350));
            signal.store(true, Ordering::SeqCst);
        });
        let start = Instant::now();
        assert_eq!(
            read_folder_size(&mut command, flag).unwrap_err(),
            "Canceled"
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        setter.join().unwrap();
    }
    #[test]
    fn folder_size_requires_complete_nonnegative_totals() {
        assert_eq!(
            parse_folder_size(br#"{"bytes":12,"count":2,"sizeless":0}"#).unwrap(),
            FolderSize {
                bytes: 12,
                count: 2
            }
        );
        assert_eq!(
            parse_folder_size(br#"{"bytes":0,"count":0}"#)
                .unwrap()
                .bytes,
            0
        );
        for invalid in [
            br#"{"bytes":12,"count":2,"sizeless":1}"#.as_slice(),
            br#"{"bytes":-1,"count":1}"#,
            br#"{"bytes":0}"#,
            b"not json",
        ] {
            assert!(parse_folder_size(invalid).is_err());
        }
    }
    #[test]
    #[ignore = "Requires RDESK_TEST_RCLONE; reads only generated local metadata"]
    fn real_rclone_folder_size() {
        let exe = std::env::var("RDESK_TEST_RCLONE").unwrap();
        let root = std::env::temp_dir().join(format!("remotedesk-size-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("nested/empty")).unwrap();
        std::fs::write(root.join("one.bin"), [1u8; 13]).unwrap();
        std::fs::write(root.join("nested/two.bin"), [2u8; 29]).unwrap();
        let mut c = process(&exe);
        c.arg("size").arg(&root).args(["--json", "--use-json-log"]);
        let result = read_folder_size(&mut c, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(
            result,
            FolderSize {
                bytes: 42,
                count: 2
            }
        );
        let mut c = process(&exe);
        c.arg("size").arg(root.join("nested/empty")).arg("--json");
        assert_eq!(
            read_folder_size(&mut c, Arc::new(AtomicBool::new(false))).unwrap(),
            FolderSize { bytes: 0, count: 0 }
        );
        let mut c = process(&exe);
        c.arg("size").arg(root.join("missing")).args([
            "--json",
            "--retries",
            "1",
            "--use-json-log",
        ]);
        assert!(read_folder_size(&mut c, Arc::new(AtomicBool::new(false))).is_err());
        let mut c = process(&exe);
        c.arg("size").arg(&root).arg("--json");
        assert_eq!(
            read_folder_size(&mut c, Arc::new(AtomicBool::new(true))).unwrap_err(),
            "Canceled"
        );
        assert_eq!(std::fs::read(root.join("one.bin")).unwrap(), [1u8; 13]);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn case_collision() {
        let a = Entry {
            path: "A".into(),
            name: "A".into(),
            size: 1,
            is_dir: false,
            mod_time: String::new(),
        };
        let mut b = a.clone();
        b.path = "a".into();
        b.name = "a".into();
        assert!(validate_listing(&[a.clone(), b.clone()]).is_ok());
        assert!(validate_manifest(&[a.clone()]).is_ok());
        assert!(validate_manifest(&[b.clone()]).is_ok());
        let error = validate_manifest(&[a, b]).unwrap_err();
        assert!(error.contains("\"A\"") && error.contains("\"a\""));
        assert!(error.contains("separately"));
    }
    #[test]
    fn normalization_collision() {
        let a = Entry {
            path: "café.txt".into(),
            name: "café.txt".into(),
            size: 1,
            is_dir: false,
            mod_time: String::new(),
        };
        let mut b = a.clone();
        b.path = "cafe\u{301}.txt".into();
        b.name = b.path.clone();
        assert!(validate_listing(&[a.clone(), b.clone()]).is_ok());
        assert!(validate_manifest(&[a, b]).is_err());
    }
    #[test]
    fn collision_validation_keeps_remote_paths_distinct_and_local_parents_safe() {
        let entry = |path: &str, is_dir| Entry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            size: 1,
            is_dir,
            mod_time: String::new(),
        };
        assert!(validate_manifest(&[
            entry("folder", true),
            entry("folder/A.txt", false),
            entry("folder/b.txt", false),
        ])
        .is_ok());
        let implicit_parents = [
            entry("Folder/first.txt", false),
            entry("folder/second.txt", false),
        ];
        assert!(validate_listing(&implicit_parents).is_ok());
        let error = validate_manifest(&implicit_parents).unwrap_err();
        assert!(error.contains("\"Folder\"") && error.contains("\"folder\""));
        assert!(validate_listing(&[entry("same", false), entry("same", false)]).is_err());
        for invalid in ["../escape", "a/../escape", "a\\escape", "CON.txt"] {
            assert!(validate_listing(&[entry(invalid, false)]).is_err());
        }
    }
    #[test]
    fn sftp_process_contract() {
        let r = Remote {
            id: "fixture".into(),
            label: "fixture".into(),
            host: "fixture.invalid".into(),
            user: "tester".into(),
            port: 2222,
            root: "/allowed".into(),
            auth: "agent".into(),
            known_hosts: "fixture.known_hosts".into(),
        };
        let s = Settings {
            rclone: "rclone".into(),
            ..Settings::default()
        };
        let hosts = std::env::temp_dir().join(format!(
            "remotedesk-host-test-{}.known_hosts",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&hosts, "fixture.invalid ssh-ed25519 AAAA\n").unwrap();
        let c = remote_command(&s, &r, &hosts).unwrap();
        std::fs::remove_file(&hosts).unwrap();
        let args: Vec<_> = c
            .get_args()
            .map(|v| v.to_string_lossy().to_string())
            .collect();
        assert!(args
            .windows(2)
            .any(|p| p == ["--config", if cfg!(windows) { "NUL" } else { "/dev/null" }]));
        let env: std::collections::HashMap<_, _> = c
            .get_envs()
            .filter_map(|(k, v)| {
                v.map(|v| {
                    (
                        k.to_string_lossy().to_string(),
                        v.to_string_lossy().to_string(),
                    )
                })
            })
            .collect();
        assert_eq!(
            env["RCLONE_CONFIG_APP_KNOWN_HOSTS_FILE"],
            hosts.to_string_lossy()
        );
        assert_eq!(env["RCLONE_CONFIG_APP_HOST_KEY_ALGORITHMS"], "ssh-ed25519");
        assert_eq!(env["RCLONE_CONFIG_APP_SKIP_LINKS"], "true");
        assert_eq!(env["RCLONE_CONFIG_APP_SHELL_TYPE"], "none");
        assert_eq!(env["RCLONE_CONFIG_APP_KEY_USE_AGENT"], "true");
        assert!(!env.contains_key("RCLONE_CONFIG_APP_PASS"));
        assert!(!args.iter().any(|v| v == "sync"));
    }
    #[test]
    fn negotiate_only_trusted_algorithms() {
        assert_eq!(
            pinned_algorithms("host ssh-ed25519 AAAA\n").unwrap(),
            "ssh-ed25519"
        );
        assert_eq!(
            pinned_algorithms("host ssh-rsa AAAA\n").unwrap(),
            "rsa-sha2-512 rsa-sha2-256"
        );
        assert!(pinned_algorithms("# empty\n@revoked host ssh-ed25519 AAAA").is_err());
        assert!(pinned_algorithms("none").is_err());
    }
    #[test]
    fn usage_is_not_an_overwrite_error() {
        assert_eq!(diagnostic("      --immutable Do not modify files"), None);
        assert!(diagnostic("unknown flag: --create-empty-src-dirs")
            .unwrap()
            .contains("rejected"));
        assert!(diagnostic(
            "Source and destination exist but do not match: immutable file modified"
        )
        .unwrap()
        .contains("differs"));
    }
    #[test]
    #[ignore = "Requires RDESK_TEST_RCLONE; copies only generated local fixtures"]
    fn real_rclone_single_file_transfer() {
        let exe = std::env::var("RDESK_TEST_RCLONE").expect("Set absolute rclone executable");
        let root =
            std::env::temp_dir().join(format!("remotedesk-file-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("Source with spaces.zip");
        let destination = root.join("destination");
        let target = destination.join("Source with spaces.zip");
        std::fs::write(&source, b"generated single-file fixture").unwrap();
        let mut c = process(&exe);
        c.arg("copyto")
            .arg(&source)
            .arg(&target)
            .arg("--use-json-log");
        transfer_options(&mut c, &Settings::default(), false);
        run(&mut c, None, Duration::from_secs(15), |_| {}).unwrap();
        assert_eq!(
            std::fs::read(&source).unwrap(),
            std::fs::read(&target).unwrap()
        );
        check_existing_file(&target, std::fs::metadata(&source).unwrap().len()).unwrap();
        std::fs::write(&target, b"preserve existing file").unwrap();
        assert!(
            check_existing_file(&target, std::fs::metadata(&source).unwrap().len())
                .unwrap_err()
                .contains("differs")
        );
        let mut c = process(&exe);
        c.arg("copyto")
            .arg(&source)
            .arg(&target)
            .args(["--use-json-log", "--retries", "1"]);
        transfer_options(&mut c, &Settings::default(), false);
        // Even if a file appears after preflight, the command must not overwrite it.
        run(&mut c, None, Duration::from_secs(15), |_| {}).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve existing file");
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    #[ignore = "Requires RDESK_TEST_RCLONE; copies only generated local fixtures"]
    fn real_rclone_copy_progress_and_cancel() {
        let exe = std::env::var("RDESK_TEST_RCLONE").expect("Set absolute rclone executable");
        assert!(validate_binary(&exe).unwrap().starts_with("rclone v1."));
        let root = std::env::temp_dir().join(format!("remotedesk-test-{}", uuid::Uuid::new_v4()));
        let src = root.join("source");
        let dst = root.join("destination");
        std::fs::create_dir_all(src.join("Example.app/Contents/empty")).unwrap();
        let filename = "Dragon's sample [fixture].bin";
        std::fs::write(src.join(filename), vec![37u8; 8 * 1024 * 1024]).unwrap();
        let mut c = process(&exe);
        c.arg("copy").arg(&src).arg(&dst).args([
            // macOS local copies can clone instantly on APFS, bypassing the
            // throttled byte stream this integration test needs to exercise.
            "--disable",
            "Copy",
            "--use-json-log",
            "--stats",
            "100ms",
            "--stats-log-level",
            "NOTICE",
            "--bwlimit",
            "4M",
            "--transfers",
            "4",
            "--multi-thread-streams",
            "16",
            "--multi-thread-cutoff",
            "1M",
            "--multi-thread-chunk-size",
            "1M",
            "--create-empty-src-dirs",
            "--immutable",
        ]);
        let mut observed = Vec::new();
        run(&mut c, None, Duration::from_secs(30), |v| observed.push(v)).unwrap();
        assert!(
            observed
                .iter()
                .any(|s| s["bytes"].as_u64().unwrap_or(0) > 0),
            "Expected actual JSON stats: {observed:?}"
        );
        assert_eq!(
            std::fs::read(dst.join(filename)).unwrap(),
            std::fs::read(src.join(filename)).unwrap()
        );
        assert!(dst.join("Example.app/Contents/empty").is_dir());
        // A differing destination must fail rather than overwrite when immutable is set.
        std::fs::write(dst.join(filename), b"preserve me").unwrap();
        let mut c = process(&exe);
        c.arg("copy")
            .arg(&src)
            .arg(&dst)
            .args(["--immutable", "--retries", "1", "--use-json-log"]);
        assert!(run(&mut c, None, Duration::from_secs(15), |_| {}).is_err());
        assert_eq!(std::fs::read(dst.join(filename)).unwrap(), b"preserve me");
        let flag = Arc::new(AtomicBool::new(false));
        let signal = flag.clone();
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(350));
            signal.store(true, Ordering::SeqCst);
        });
        let mut c = process(&exe);
        c.arg("copy").arg(&src).arg(root.join("canceled")).args([
            "--disable",
            "Copy",
            "--bwlimit",
            "16k",
            "--use-json-log",
        ]);
        let start = Instant::now();
        assert_eq!(
            run(&mut c, Some(flag), Duration::from_secs(15), |_| {}).unwrap_err(),
            "Canceled"
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        setter.join().unwrap();
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
