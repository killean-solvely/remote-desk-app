#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod credentials;
mod engine;
mod hostkeys;
mod model;
mod updates;
use model::*;
use rusqlite::{params, Connection};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::{Emitter, Manager};

struct Inner {
    db: Mutex<Connection>,
    mutations: Mutex<()>,
    connection_tasks: Mutex<HashMap<String, usize>>,
    host_key_candidates: Mutex<HashMap<String, hostkeys::PendingKey>>,
    data: PathBuf,
    bundled_engine: Mutex<Option<String>>,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    size_tasks: Mutex<HashMap<String, Arc<AtomicBool>>>,
    stop: AtomicBool,
    updating: AtomicBool,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    _lock: std::fs::File,
}
#[derive(Clone)]
struct State(Arc<Inner>);
// Prevent connection/credential edits while an old connection snapshot is in use.
struct ConnectionTask {
    state: State,
    id: String,
}
impl Drop for ConnectionTask {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.state.0.connection_tasks.lock() {
            if let Some(count) = tasks.get_mut(&self.id) {
                *count -= 1;
                if *count == 0 {
                    tasks.remove(&self.id);
                }
            }
        }
    }
}
type Result<T> = std::result::Result<T, String>;
impl State {
    fn open(data: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data).map_err(|_| "Cannot create app data directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "Cannot restrict app data permissions")?;
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data.join("instance.lock"))
            .map_err(|_| "Cannot open instance lock")?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| "Remote Desk is already running")?;
        let db =
            Connection::open(data.join("state.sqlite3")).map_err(|_| "Cannot open database")?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,id TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(kind,id)); PRAGMA user_version=1;").map_err(|_|"Cannot initialize database")?;
        let s = State(Arc::new(Inner {
            db: Mutex::new(db),
            mutations: Mutex::new(()),
            connection_tasks: Mutex::new(HashMap::new()),
            host_key_candidates: Mutex::new(HashMap::new()),
            data,
            bundled_engine: Mutex::new(None),
            cancels: Mutex::new(HashMap::new()),
            size_tasks: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            updating: AtomicBool::new(false),
            worker: Mutex::new(None),
            _lock: lock,
        }));
        for mut j in s.all::<Job>("job")? {
            if ["queued", "preparing", "running"].contains(&j.state.as_str()) {
                j.state = "interrupted".into();
                j.detail =
                    "Application exited; retry explicitly to recheck and skip completed files"
                        .into();
                s.put("job", &j.id, &j)?;
            }
        }
        Ok(s)
    }
    fn allow_work(&self) -> Result<()> {
        if self.0.updating.load(Ordering::SeqCst) {
            return Err("An app update is being installed. Please wait.".into());
        }
        Ok(())
    }
    fn put<T: Serialize>(&self, kind: &str, id: &str, v: &T) -> Result<()> {
        self.0.db.lock().map_err(|_|"Database lock failed")?.execute("INSERT INTO records(kind,id,data) VALUES(?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET data=excluded.data",params![kind,id,serde_json::to_string(v).map_err(|_|"Cannot serialize record")?]).map_err(|_|"Cannot save database")?;
        Ok(())
    }
    fn all<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>> {
        let db = self.0.db.lock().map_err(|_| "Database lock failed")?;
        let mut stmt = db
            .prepare("SELECT data FROM records WHERE kind=?1 ORDER BY rowid")
            .map_err(|_| "Cannot query database")?;
        let rows = stmt
            .query_map([kind], |r| r.get::<_, String>(0))
            .map_err(|_| "Cannot query records")?;
        rows.map(|r| {
            serde_json::from_str(&r.map_err(|_| "Cannot read record")?)
                .map_err(|_| "Invalid stored record".into())
        })
        .collect()
    }
    fn lease_remote(&self, id: &str) -> Result<(Remote, ConnectionTask)> {
        let _guard = self.0.mutations.lock().map_err(|_| "Connection is busy")?;
        self.allow_work()?;
        let remote = self.remote(id)?;
        *self
            .0
            .connection_tasks
            .lock()
            .map_err(|_| "Connection is busy")?
            .entry(id.into())
            .or_default() += 1;
        Ok((
            remote,
            ConnectionTask {
                state: self.clone(),
                id: id.into(),
            },
        ))
    }
    fn idle_remote(&self, id: &str) -> Result<Remote> {
        let remote = self.remote(id)?;
        if self
            .0
            .connection_tasks
            .lock()
            .map_err(|_| "Connection is busy")?
            .contains_key(id)
        {
            return Err(
                "Wait for this connection’s listing or connection test to finish first".into(),
            );
        }
        if self
            .all::<Job>("job")?
            .iter()
            .any(|j| j.remote_id == id && active_job(&j.state))
        {
            return Err(
                "Finish or cancel this connection's queued and active downloads first".into(),
            );
        }
        Ok(remote)
    }
    fn remove_history(&self, id: &str) -> Result<()> {
        let job = self
            .all::<Job>("job")?
            .into_iter()
            .find(|j| j.id == id)
            .ok_or("Job not found")?;
        if active_job(&job.state) {
            return Err("Active downloads cannot be removed from history".into());
        }
        let mut db = self.0.db.lock().map_err(|_| "Database lock failed")?;
        let tx = db.transaction().map_err(|_| "Cannot update history")?;
        tx.execute(
            "DELETE FROM records WHERE id=?1 AND kind IN ('job','manifest')",
            [id],
        )
        .map_err(|_| "Cannot remove history")?;
        tx.commit().map_err(|_| "Cannot save history")?;
        Ok(())
    }
    fn remote(&self, id: &str) -> Result<Remote> {
        self.all::<Remote>("remote")?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or("Remote not found".into())
    }
    fn settings(&self) -> Result<Settings> {
        let mut settings = self
            .all::<Settings>("settings")?
            .into_iter()
            .next()
            .unwrap_or_default();
        if let Some(path) = self
            .0
            .bundled_engine
            .lock()
            .map_err(|_| "Engine is busy")?
            .as_ref()
        {
            settings.rclone = path.clone();
        }
        Ok(settings)
    }
    fn use_bundled_engine(&self, resources: &Path) -> Result<()> {
        let path = resources.join("engine").join(if cfg!(windows) {
            "rclone.exe"
        } else {
            "rclone"
        });
        *self.0.bundled_engine.lock().map_err(|_| "Engine is busy")? =
            Some(path.to_string_lossy().into_owned());
        // Remove obsolete absolute paths while preserving every transfer preference.
        let mut stored = self.settings()?;
        stored.rclone.clear();
        self.put("settings", "main", &stored)
    }
    fn hosts(&self, r: &Remote) -> PathBuf {
        self.0.data.join(&r.known_hosts)
    }
    fn update(&self, app: &tauri::AppHandle, j: &Job, event: &str) {
        if self.put("job", &j.id, j).is_ok() {
            let _ = app.emit(event, j);
        }
    }
}
fn active_job(state: &str) -> bool {
    ["queued", "preparing", "running"].contains(&state)
}
fn validate_remote(remote: &mut Remote) -> Result<()> {
    if remote.host.is_empty()
        || remote
            .host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
        || remote.user.is_empty()
        || remote
            .user
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
        || remote.port == 0
    {
        return Err("Provide a valid host, SSH port and username without spaces".into());
    }
    if !["agent", "password"].contains(&remote.auth.as_str()) {
        return Err("Choose SSH agent or OS-stored password".into());
    }
    let root = remote.root.trim_end_matches('/');
    let relative_root = root.trim_start_matches('/');
    if !relative_root.is_empty() {
        relative(relative_root)?;
    }
    remote.root = if remote.root == "/" {
        "/".into()
    } else {
        root.into()
    };
    Ok(())
}
fn resolve_hosts(
    s: &State,
    remote: &Remote,
    verified: bool,
    token: Option<&str>,
) -> Result<String> {
    if let Some(token) = token {
        let candidates =
            s.0.host_key_candidates
                .lock()
                .map_err(|_| "Host-key lookup is busy")?;
        return candidates
            .get(token)
            .ok_or("Fetch the host key again before saving")?
            .verified_line(&remote.host, remote.port, verified);
    }
    verified_hosts(&remote.known_hosts, verified)
}
#[tauri::command]
async fn fetch_host_key(
    s: tauri::State<'_, State>,
    host: String,
    port: u16,
) -> Result<hostkeys::HostKey> {
    let (key, pending) = hostkeys::fetch(host, port).await?;
    let mut candidates =
        s.0.host_key_candidates
            .lock()
            .map_err(|_| "Host-key lookup is busy")?;
    candidates.retain(|_, value| value.fetched.elapsed() < Duration::from_secs(600));
    if candidates.len() >= 32 {
        candidates.clear();
    }
    candidates.insert(key.token.clone(), pending);
    Ok(key)
}
fn verified_hosts(path: &str, verified: bool) -> Result<String> {
    if !verified {
        return Err(
            "Verify host-key fingerprints through a trusted independent channel first".into(),
        );
    }
    let hosts =
        std::fs::read_to_string(path).map_err(|_| "Cannot read verified known_hosts file")?;
    if hosts.len() > 1024 * 1024 || hosts.trim().is_empty() {
        return Err("Choose a nonempty known_hosts file (maximum 1 MiB)".into());
    }
    engine::pinned_algorithms(&hosts)?;
    Ok(hosts)
}
fn job_state(
    s: &State,
    app: &tauri::AppHandle,
    j: &mut Job,
    status: &str,
    detail: &str,
    event: &str,
) {
    j.state = status.into();
    j.detail = detail.into();
    s.update(app, j, event);
}
fn transfer(s: &State, app: &tauri::AppHandle, j: &mut Job, cancel: Arc<AtomicBool>) -> Result<()> {
    let r = s.remote(&j.remote_id)?;
    let settings = s.settings()?;
    let hosts = s.hosts(&r);
    let destination_path = j.destination.clone();
    let destination = Path::new(&destination_path);
    engine::clean_ancestors(destination)?;
    engine::clean_tree(destination)?;
    let mut stat = engine::remote_command(&settings, &r, &hosts)?;
    stat.args(["lsjson", &source(&r, &j.path)?, "--stat", "--no-mimetype"]);
    let (data, _) = engine::run(
        &mut stat,
        Some(cancel.clone()),
        Duration::from_secs(60),
        |_| {},
    )?;
    let entry: Entry = serde_json::from_slice(&data).map_err(|_| "Invalid remote metadata")?;
    let manifest = if entry.is_dir {
        engine::list(&settings, &r, &hosts, &j.path, true, Some(cancel.clone()))?
    } else {
        vec![Entry {
            path: entry.name.clone(),
            ..entry.clone()
        }]
    };
    engine::validate_manifest(&manifest)?;
    let bytes = manifest
        .iter()
        .filter(|e| !e.is_dir)
        .try_fold(0u64, |n, e| n.checked_add(e.size.max(0) as u64))
        .ok_or("Source size overflow")?;
    if fs2::available_space(destination).map_err(|_| "Cannot check destination free space")? < bytes
    {
        return Err(
            "Not enough free space (conservative full-source estimate, including retries)".into(),
        );
    }
    let target = destination.join(j.path.rsplit('/').next().ok_or("Select a file or folder")?);
    if !entry.is_dir && target.exists() {
        engine::check_existing_file(&target, entry.size.max(0) as u64)?;
    }
    let mut command = engine::remote_command(&settings, &r, &hosts)?;
    command
        .arg(if entry.is_dir { "copy" } else { "copyto" })
        .arg(source(&r, &j.path)?)
        .arg(&target);
    engine::transfer_options(&mut command, &settings, entry.is_dir);
    job_state(
        s,
        app,
        j,
        "running",
        "Downloading; final verification uses file sizes",
        "job_started",
    );
    engine::run(
        &mut command,
        Some(cancel.clone()),
        Duration::from_secs(7 * 86400),
        |stats| {
            j.stats = stats;
            s.update(app, j, "job_progress");
            if let Some(files) = j.stats.get("transferring") {
                let _ = app.emit("file_progress", json!({"id":j.id,"files":files}));
            }
        },
    )?;
    if cancel.load(Ordering::SeqCst) {
        return Err("Canceled".into());
    }
    engine::clean_tree(destination)?;
    for e in &manifest {
        let path = if entry.is_dir {
            target.join(&e.path)
        } else {
            target.clone()
        };
        let m = std::fs::metadata(path).map_err(|_| "Downloaded item is missing")?;
        if (e.is_dir && !m.is_dir())
            || (!e.is_dir && (!m.is_file() || m.len() != e.size.max(0) as u64))
        {
            return Err("Downloaded size or type does not match the source manifest".into());
        }
    }
    s.put("manifest", &j.id, &manifest)?;
    Ok(())
}
fn worker(s: State, app: tauri::AppHandle) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !s.0.stop.load(Ordering::SeqCst) {
            let claim = s.0.mutations.lock().unwrap();
            let next = s
                .all::<Job>("job")
                .unwrap_or_default()
                .into_iter()
                .find(|j| j.state == "queued");
            if let Some(mut j) = next {
                let flag = {
                    let mut flags = s.0.cancels.lock().unwrap();
                    flags
                        .entry(j.id.clone())
                        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                        .clone()
                };
                if s.0.stop.load(Ordering::SeqCst) {
                    flag.store(true, Ordering::SeqCst);
                }
                if flag.load(Ordering::SeqCst) {
                    job_state(
                        &s,
                        &app,
                        &mut j,
                        "canceled",
                        "Canceled before start",
                        "job_canceled",
                    );
                    continue;
                }
                job_state(
                    &s,
                    &app,
                    &mut j,
                    "preparing",
                    "Validating source names and free space",
                    "job_progress",
                );
                drop(claim);
                match transfer(&s, &app, &mut j, flag.clone()) {
                    Ok(()) => job_state(
                        &s,
                        &app,
                        &mut j,
                        "completed",
                        "Verified local sizes; no checksum guarantee",
                        "job_completed",
                    ),
                    Err(e) => {
                        let canceled = flag.load(Ordering::SeqCst);
                        job_state(
                            &s,
                            &app,
                            &mut j,
                            if canceled { "canceled" } else { "failed" },
                            &e,
                            if canceled {
                                "job_canceled"
                            } else {
                                "job_failed"
                            },
                        )
                    }
                }
                let mut flags = s.0.cancels.lock().unwrap();
                if flags
                    .get(&j.id)
                    .is_some_and(|current| Arc::ptr_eq(current, &flag))
                {
                    flags.remove(&j.id);
                }
            } else {
                drop(claim);
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    })
}
#[tauri::command]
fn list_remotes(s: tauri::State<State>) -> Result<Vec<Remote>> {
    s.all("remote")
}
#[tauri::command]
fn get_settings(s: tauri::State<State>) -> Result<Settings> {
    s.settings()
}
#[tauri::command]
fn list_jobs(s: tauri::State<State>) -> Result<Vec<Job>> {
    s.all("job")
}
#[tauri::command]
fn get_logs(s: tauri::State<State>) -> Result<Vec<Value>> {
    Ok(s.all::<Job>("job")?
        .iter()
        .map(|j| json!({"id":j.id,"state":j.state,"detail":j.detail}))
        .collect())
}
#[tauri::command]
async fn select_destination(app: tauri::AppHandle) -> Result<Option<String>> {
    let downloads = app.path().download_dir().ok();
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new().set_title("Choose download destination");
        if let Some(directory) = downloads.filter(|p| p.is_dir()) {
            dialog = dialog.set_directory(directory);
        }
        Ok(dialog
            .pick_folder()
            .map(|p| p.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|_| "Dialog failed")?
}
#[tauri::command]
async fn select_file() -> Result<Option<String>> {
    tauri::async_runtime::spawn_blocking(|| {
        Ok(rfd::FileDialog::new()
            .pick_file()
            .map(|p| p.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|_| "Dialog failed")?
}
#[tauri::command]
async fn set_settings(s: tauri::State<'_, State>, mut settings: Settings) -> Result<String> {
    let s = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if !(1..=16).contains(&settings.transfers) || !(1..=32).contains(&settings.streams) {
            return Err("Transfers must be 1–16; streams must be 1–32".into());
        }
        for size in [&settings.cutoff, &settings.chunk] {
            if size.is_empty()
                || !size
                    .chars()
                    .all(|c| c.is_ascii_digit() || "KMGkmg".contains(c))
            {
                return Err("Use a size such as 100M or 16M".into());
            }
        }
        if !settings
            .bandwidth
            .chars()
            .all(|c| c.is_ascii_digit() || "KMGkmg.".contains(c))
        {
            return Err("Bandwidth must be empty or a rate such as 10M".into());
        }
        if s.0
            .bundled_engine
            .lock()
            .map_err(|_| "Engine is busy")?
            .is_some()
        {
            settings.rclone = s.settings()?.rclone;
        }
        let version = engine::validate_binary(&settings.rclone)?;
        settings.rclone.clear();
        s.put("settings", "main", &settings)?;
        Ok(version)
    })
    .await
    .map_err(|_| "Settings task failed")?
}
#[tauri::command]
fn add_remote(
    s: tauri::State<State>,
    mut remote: Remote,
    verified: bool,
    host_key_token: Option<String>,
) -> Result<Remote> {
    let _guard = s.0.mutations.lock().map_err(|_| "Connection is busy")?;
    s.allow_work()?;
    validate_remote(&mut remote)?;
    let hosts = resolve_hosts(&s, &remote, verified, host_key_token.as_deref())?;
    remote.id = uuid::Uuid::new_v4().to_string();
    remote.known_hosts = format!("{}.known_hosts", remote.id);
    std::fs::write(s.hosts(&remote), hosts).map_err(|_| "Cannot snapshot host keys")?;
    s.put("remote", &remote.id, &remote)?;
    Ok(remote)
}
fn update_connection(
    s: &State,
    remote: Remote,
    verified: bool,
    replace_hosts: bool,
    host_key_token: Option<String>,
) -> Result<Remote> {
    let _guard = s.0.mutations.lock().map_err(|_| "Connection is busy")?;
    s.allow_work()?;
    let old = s.idle_remote(&remote.id)?;
    let mut remote = remote;
    validate_remote(&mut remote)?;
    let identity_changed =
        old.host != remote.host || old.port != remote.port || old.user != remote.user;
    if (old.host != remote.host || old.port != remote.port) && !replace_hosts {
        return Err(
            "Changing the host or port requires fetching and verifying its host key".into(),
        );
    }
    let hosts = if replace_hosts {
        Some(resolve_hosts(
            &s,
            &remote,
            verified,
            host_key_token.as_deref(),
        )?)
    } else {
        None
    };
    remote.known_hosts = old.known_hosts.clone();
    if let Some(hosts) = hosts {
        remote.known_hosts = format!("{}-{}.known_hosts", remote.id, uuid::Uuid::new_v4());
        std::fs::write(s.hosts(&remote), hosts).map_err(|_| "Cannot snapshot host keys")?;
    }
    if identity_changed || remote.auth != old.auth {
        credentials::delete(&remote.id)?;
    }
    s.put("remote", &remote.id, &remote)?;
    if old.known_hosts != remote.known_hosts {
        let _ = std::fs::remove_file(s.hosts(&old));
    }
    Ok(remote)
}
#[tauri::command]
async fn update_remote(
    s: tauri::State<'_, State>,
    remote: Remote,
    verified: bool,
    replace_hosts: bool,
    host_key_token: Option<String>,
) -> Result<Remote> {
    let s = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        update_connection(&s, remote, verified, replace_hosts, host_key_token)
    })
    .await
    .map_err(|_| "Connection update failed")?
}
fn delete_connection(s: &State, remote_id: &str) -> Result<()> {
    let _guard = s.0.mutations.lock().map_err(|_| "Connection is busy")?;
    s.allow_work()?;
    let remote = s.idle_remote(&remote_id)?;
    credentials::delete(&remote_id)?;
    s.0.db
        .lock()
        .map_err(|_| "Database lock failed")?
        .execute(
            "DELETE FROM records WHERE kind='remote' AND id=?1",
            [&remote_id],
        )
        .map_err(|_| "Cannot delete connection")?;
    let _ = std::fs::remove_file(s.hosts(&remote));
    Ok(())
}
#[tauri::command]
async fn delete_remote(s: tauri::State<'_, State>, remote_id: String) -> Result<()> {
    let s = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || delete_connection(&s, &remote_id))
        .await
        .map_err(|_| "Connection deletion failed")?
}
#[tauri::command]
async fn set_remote_password(
    s: tauri::State<'_, State>,
    window: tauri::WebviewWindow,
    remote_id: String,
) -> Result<bool> {
    let s = s.inner().clone();
    #[cfg(windows)]
    let parent = window.hwnd().map_err(|_| "Cannot identify app window")?.0 as isize;
    #[cfg(not(windows))]
    let parent = {
        let _ = window;
        0
    };
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = s.0.mutations.lock().map_err(|_| "Connection is busy")?;
        s.allow_work()?;
        let remote = s.idle_remote(&remote_id)?;
        if remote.auth != "password" {
            return Err("Choose password authentication before setting a password".into());
        }
        let settings = s.settings()?;
        engine::validate_binary(&settings.rclone)?;
        match credentials::prompt(&remote.user, &remote.host, parent)? {
            Some(secret) => {
                engine::store_password(&remote_id, &settings.rclone, secret)?;
                Ok(true)
            }
            None => Ok(false),
        }
    })
    .await
    .map_err(|_| "Password setup failed")?
}
#[tauri::command]
fn delete_history(s: tauri::State<State>, id: String) -> Result<()> {
    let _guard = s.0.mutations.lock().map_err(|_| "History is busy")?;
    s.allow_work()?;
    s.remove_history(&id)
}
#[tauri::command]
fn clear_completed(s: tauri::State<State>) -> Result<usize> {
    let _guard = s.0.mutations.lock().map_err(|_| "History is busy")?;
    s.allow_work()?;
    let completed = s
        .all::<Job>("job")?
        .into_iter()
        .filter(|j| j.state == "completed")
        .collect::<Vec<_>>();
    for job in &completed {
        s.remove_history(&job.id)?;
    }
    Ok(completed.len())
}
#[tauri::command]
async fn list_directory(
    s: tauri::State<'_, State>,
    remote_id: String,
    path: String,
) -> Result<Vec<Entry>> {
    let s = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (r, _connection_task) = s.lease_remote(&remote_id)?;
        engine::list(&s.settings()?, &r, &s.hosts(&r), &path, false, None)
    })
    .await
    .map_err(|_| "Listing task failed")?
}
// Keep a scan registered until its blocking worker exits, including failures.
struct SizeTask {
    state: State,
    id: String,
}
impl Drop for SizeTask {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.state.0.size_tasks.lock() {
            tasks.remove(&self.id);
        }
    }
}
#[tauri::command]
async fn calculate_folder_size(
    s: tauri::State<'_, State>,
    remote_id: String,
    path: String,
    request_id: String,
) -> Result<engine::FolderSize> {
    uuid::Uuid::parse_str(&request_id).map_err(|_| "Invalid size request")?;
    relative(&path)?;
    let s = s.inner().clone();
    let (r, connection_task) = s.lease_remote(&remote_id)?;
    let flag = Arc::new(AtomicBool::new(false));
    {
        let mut tasks =
            s.0.size_tasks
                .lock()
                .map_err(|_| "Folder size scan unavailable")?;
        if !tasks.is_empty() {
            return Err("Finish or cancel the current folder size scan first".into());
        }
        tasks.insert(request_id.clone(), flag.clone());
    }
    let task = SizeTask {
        state: s.clone(),
        id: request_id,
    };
    tauri::async_runtime::spawn_blocking(move || {
        let _connection_task = connection_task;
        let _task = task;
        engine::folder_size(&s.settings()?, &r, &s.hosts(&r), &path, flag)
    })
    .await
    .map_err(|_| "Folder size task failed")?
}
#[tauri::command]
fn cancel_folder_size(s: tauri::State<State>, request_id: String) -> Result<()> {
    if let Some(flag) =
        s.0.size_tasks
            .lock()
            .map_err(|_| "Folder size scan unavailable")?
            .get(&request_id)
    {
        flag.store(true, Ordering::SeqCst);
    }
    Ok(())
}
#[tauri::command]
async fn test_remote(s: tauri::State<'_, State>, remote_id: String) -> Result<String> {
    let s = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (r, _connection_task) = s.lease_remote(&remote_id)?;
        engine::list(&s.settings()?, &r, &s.hosts(&r), "", false, None)?;
        Ok("Connected with strict host-key validation".into())
    })
    .await
    .map_err(|_| "Connection task failed")?
}
#[tauri::command]
async fn get_metadata(
    s: tauri::State<'_, State>,
    remote_id: String,
    path: String,
) -> Result<Entry> {
    let s = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (r, _connection_task) = s.lease_remote(&remote_id)?;
        let mut c = engine::remote_command(&s.settings()?, &r, &s.hosts(&r))?;
        c.args(["lsjson", &source(&r, &path)?, "--stat", "--no-mimetype"]);
        let (b, _) = engine::run(&mut c, None, Duration::from_secs(60), |_| {})?;
        serde_json::from_slice(&b).map_err(|_| "Invalid metadata".into())
    })
    .await
    .map_err(|_| "Metadata task failed")?
}
#[tauri::command]
fn enqueue_download(
    s: tauri::State<State>,
    remote_id: String,
    path: String,
    destination: String,
) -> Result<Job> {
    let _guard = s.0.mutations.lock().map_err(|_| "Connection is busy")?;
    s.allow_work()?;
    s.remote(&remote_id)?;
    relative(&path)?;
    if path.is_empty() {
        return Err("Select a file or folder inside the remote root".into());
    }
    engine::clean_ancestors(Path::new(&destination))?;
    let parent =
        std::fs::canonicalize(&destination).map_err(|_| "Select an existing local destination")?;
    if !parent.is_dir() {
        return Err("Destination must be a folder".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let folder = parent.join(format!("RemoteDesk-{id}"));
    std::fs::create_dir(&folder).map_err(|_| "Cannot create isolated download folder")?;
    let j = Job {
        id: id.clone(),
        remote_id,
        path,
        destination: folder.to_string_lossy().into_owned(),
        state: "queued".into(),
        detail: "Queued; existing user files are protected by a new job folder".into(),
        stats: json!({}),
    };
    s.0.cancels
        .lock()
        .unwrap()
        .insert(id.clone(), Arc::new(AtomicBool::new(false)));
    s.put("job", &id, &j)?;
    Ok(j)
}
#[tauri::command]
fn cancel_job(s: tauri::State<State>, id: String) -> Result<()> {
    let flags = s.0.cancels.lock().map_err(|_| "Queue unavailable")?;
    let flag = flags.get(&id).ok_or("Job is no longer active")?;
    flag.store(true, Ordering::SeqCst);
    Ok(())
}
#[tauri::command]
fn retry_job(s: tauri::State<State>, id: String) -> Result<()> {
    let _guard = s.0.mutations.lock().map_err(|_| "Connection is busy")?;
    s.allow_work()?;
    let mut j = s
        .all::<Job>("job")?
        .into_iter()
        .find(|j| j.id == id)
        .ok_or("Job not found")?;
    if !retryable(&j.state) {
        return Err("Only failed, canceled or interrupted jobs can be retried".into());
    }
    s.remote(&j.remote_id)
        .map_err(|_| "This connection was deleted; add a connection and queue a new download")?;
    engine::clean_tree(Path::new(&j.destination))?;
    s.0.cancels
        .lock()
        .unwrap()
        .insert(id, Arc::new(AtomicBool::new(false)));
    j.state = "queued".into();
    j.stats = json!({});
    s.put("job", &j.id, &j)
}
fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let s = State::open(app.path().app_data_dir()?).map_err(std::io::Error::other)?;
            s.use_bundled_engine(&app.path().resource_dir()?)
                .map_err(std::io::Error::other)?;
            app.manage(s.clone());
            let handle = worker(s.clone(), app.handle().clone());
            *s.0.worker.lock().unwrap() = Some(handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            updates::check_update,
            updates::install_update,
            list_remotes,
            add_remote,
            fetch_host_key,
            update_remote,
            delete_remote,
            set_remote_password,
            delete_history,
            clear_completed,
            test_remote,
            list_directory,
            calculate_folder_size,
            cancel_folder_size,
            get_metadata,
            enqueue_download,
            list_jobs,
            cancel_job,
            retry_job,
            get_settings,
            set_settings,
            select_destination,
            select_file,
            get_logs
        ])
        .build(tauri::generate_context!())
        .expect("Cannot start desktop app");
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            let s = app.state::<State>();
            for flag in s.0.size_tasks.lock().unwrap().values() {
                flag.store(true, Ordering::SeqCst);
            }
            s.0.stop.store(true, Ordering::SeqCst);
            for flag in s.0.cancels.lock().unwrap().values() {
                flag.store(true, Ordering::SeqCst);
            }
            if let Some(handle) = s.0.worker.lock().unwrap().take() {
                let _ = handle.join();
            };
        }
    });
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    fn fixture_remote() -> Remote {
        Remote {
            id: uuid::Uuid::new_v4().to_string(),
            label: "Local fixture".into(),
            host: "example.invalid".into(),
            port: 22,
            user: "fixture".into(),
            root: "/data".into(),
            auth: "agent".into(),
            known_hosts: "fixture.known_hosts".into(),
        }
    }
    #[test]
    fn connection_edits_keep_identity_and_pins_and_require_verified_host_changes() {
        let data =
            std::env::temp_dir().join(format!("remotedesk-edit-test-{}", uuid::Uuid::new_v4()));
        {
            let s = State::open(data.clone()).unwrap();
            let mut r = fixture_remote();
            std::fs::write(s.hosts(&r), "example.invalid ssh-ed25519 public-fixture").unwrap();
            s.put("remote", &r.id, &r).unwrap();
            let id = r.id.clone();
            r.label = "Renamed".into();
            r.user = "corrected".into();
            r.root = "/new-root/".into();
            r.known_hosts.clear();
            let mut saved = update_connection(&s, r, false, false, None).unwrap();
            let (_, task) = s.lease_remote(&id).unwrap();
            assert!(update_connection(&s, saved.clone(), false, false, None).is_err());
            assert!(delete_connection(&s, &id).is_err());
            drop(task);
            assert_eq!(saved.id, id);
            assert_eq!(saved.root, "/new-root");
            assert_eq!(saved.known_hosts, "fixture.known_hosts");
            assert!(s.hosts(&saved).exists());
            saved.host = "new.example.invalid".into();
            assert!(update_connection(&s, saved.clone(), false, false, None).is_err());
            assert_eq!(s.remote(&id).unwrap().host, "example.invalid");
            let replacement = data.join("replacement");
            std::fs::write(
                &replacement,
                "new.example.invalid ssh-ed25519 verified-fixture",
            )
            .unwrap();
            saved.known_hosts = replacement.to_string_lossy().into_owned();
            assert!(update_connection(&s, saved.clone(), false, true, None).is_err());
            let changed = update_connection(&s, saved, true, true, None).unwrap();
            assert_eq!(changed.id, id);
            assert!(s.hosts(&changed).exists());
            assert!(!data.join("fixture.known_hosts").exists());
        }
        std::fs::remove_dir_all(data).unwrap();
    }
    #[test]
    fn installed_engine_migrates_old_paths_and_follows_app_moves() {
        let data =
            std::env::temp_dir().join(format!("remotedesk-install-test-{}", uuid::Uuid::new_v4()));
        {
            let s = State::open(data.clone()).unwrap();
            let mut old = Settings::default();
            old.rclone = "/obsolete/Downloads/rclone".into();
            old.streams = 8;
            s.put("settings", "main", &old).unwrap();
            let remote = fixture_remote();
            s.put("remote", &remote.id, &remote).unwrap();
            s.use_bundled_engine(&data.join("Mounted DMG/Resources"))
                .unwrap();
            assert!(s.settings().unwrap().rclone.contains("Mounted DMG"));
            s.use_bundled_engine(&data.join("Applications/Resources"))
                .unwrap();
            let updated = s.settings().unwrap();
            assert_eq!(updated.streams, 8);
            assert_eq!(
                Path::new(&updated.rclone),
                data.join("Applications/Resources/engine")
                    .join(if cfg!(windows) {
                        "rclone.exe"
                    } else {
                        "rclone"
                    })
            );
            assert!(s.all::<Settings>("settings").unwrap()[0].rclone.is_empty());
            assert_eq!(s.remote(&remote.id).unwrap().user, remote.user);
        }
        std::fs::remove_dir_all(data).unwrap();
    }
    #[test]
    fn fetched_pin_requires_confirmation_and_matching_endpoint() {
        let data =
            std::env::temp_dir().join(format!("remotedesk-pin-test-{}", uuid::Uuid::new_v4()));
        {
            let s = State::open(data.clone()).unwrap();
            let mut remote = fixture_remote();
            std::fs::write(s.hosts(&remote), "old pins").unwrap();
            s.put("remote", &remote.id, &remote).unwrap();
            let token = uuid::Uuid::new_v4().to_string();
            let line = "example.invalid ssh-ed25519 fixture-public-key\n";
            s.0.host_key_candidates.lock().unwrap().insert(
                token.clone(),
                hostkeys::PendingKey {
                    host: remote.host.clone(),
                    port: remote.port,
                    line: line.into(),
                    fetched: std::time::Instant::now(),
                },
            );
            remote.known_hosts = "must not read this frontend path".into();
            assert!(
                update_connection(&s, remote.clone(), false, true, Some(token.clone())).is_err()
            );
            assert!(
                update_connection(&s, remote.clone(), true, true, Some("invalid".into())).is_err()
            );
            remote.port = 2222;
            assert!(
                update_connection(&s, remote.clone(), true, true, Some(token.clone())).is_err()
            );
            remote.port = 22;
            let saved = update_connection(&s, remote, true, true, Some(token)).unwrap();
            assert_eq!(std::fs::read_to_string(s.hosts(&saved)).unwrap(), line);
        }
        std::fs::remove_dir_all(data).unwrap();
    }
    #[test]
    fn deletion_preserves_files_blocks_active_jobs_and_removes_manifests() {
        let data =
            std::env::temp_dir().join(format!("remotedesk-delete-test-{}", uuid::Uuid::new_v4()));
        {
            let s = State::open(data.clone()).unwrap();
            let remote = fixture_remote();
            s.put("remote", &remote.id, &remote).unwrap();
            std::fs::write(s.hosts(&remote), "fixture pins").unwrap();
            let download = data.join("keep-download.zip");
            std::fs::write(&download, b"keep me").unwrap();
            let mut job = Job {
                id: uuid::Uuid::new_v4().to_string(),
                remote_id: remote.id.clone(),
                path: "archive.zip".into(),
                destination: download.to_string_lossy().into_owned(),
                state: "queued".into(),
                detail: String::new(),
                stats: json!({}),
            };
            for state in ["queued", "preparing", "running"] {
                job.state = state.into();
                s.put("job", &job.id, &job).unwrap();
                assert!(s.remove_history(&job.id).is_err());
                assert!(delete_connection(&s, &remote.id).is_err());
                assert!(update_connection(&s, remote.clone(), false, false, None).is_err());
            }
            job.state = "completed".into();
            s.put("job", &job.id, &job).unwrap();
            s.put("manifest", &job.id, &json!([{"Path":"archive.zip"}]))
                .unwrap();
            delete_connection(&s, &remote.id).unwrap();
            assert!(s.remote(&remote.id).is_err());
            assert!(!s.hosts(&remote).exists());
            assert_eq!(s.all::<Job>("job").unwrap().len(), 1);
            s.remove_history(&job.id).unwrap();
            assert!(s.all::<Job>("job").unwrap().is_empty());
            assert!(s.all::<Value>("manifest").unwrap().is_empty());
            assert_eq!(std::fs::read(download).unwrap(), b"keep me");
        }
        std::fs::remove_dir_all(data).unwrap();
    }
    #[test]
    fn restart_recovers_queue_and_excludes_second_instance() {
        let data =
            std::env::temp_dir().join(format!("remotedesk-db-test-{}", uuid::Uuid::new_v4()));
        {
            let s = State::open(data.clone()).unwrap();
            assert!(State::open(data.clone()).is_err());
            for state in [
                "queued",
                "preparing",
                "running",
                "completed",
                "failed",
                "canceled",
            ] {
                let j = Job {
                    id: state.into(),
                    remote_id: "fixture".into(),
                    path: "fixture".into(),
                    destination: "fixture".into(),
                    state: state.into(),
                    detail: String::new(),
                    stats: json!({}),
                };
                s.put("job", state, &j).unwrap();
            }
            s.put("settings", "main", &Settings::default()).unwrap();
        }
        {
            let s = State::open(data.clone()).unwrap();
            let jobs = s.all::<Job>("job").unwrap();
            assert_eq!(jobs.iter().filter(|j| j.state == "interrupted").count(), 3);
            assert!(jobs
                .iter()
                .all(|j| !["queued", "preparing", "running"].contains(&j.state.as_str())));
            assert_eq!(
                jobs.iter().find(|j| j.id == "completed").unwrap().state,
                "completed"
            );
            assert_eq!(s.settings().unwrap().streams, 16);
        }
        assert!(data.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(data).unwrap();
    }
}
