use crate::{Job, Result, State};
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    current_version: String,
    version: Option<String>,
    notes: Option<String>,
}

#[tauri::command]
pub async fn check_update(app: tauri::AppHandle) -> Result<UpdateInfo> {
    let update = app
        .updater_builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Cannot initialize update checking")?
        .check()
        .await
        .map_err(|_| {
            "Could not check for updates. Check your internet connection and try again."
        })?;
    Ok(UpdateInfo {
        current_version: app.package_info().version.to_string(),
        version: update.as_ref().map(|u| u.version.clone()),
        notes: update.and_then(|u| u.body),
    })
}

// Holds the queue gate across network awaits without holding a synchronous mutex.
struct UpdateGuard(State);
impl Drop for UpdateGuard {
    fn drop(&mut self) {
        self.0 .0.updating.store(false, Ordering::SeqCst);
    }
}
fn begin(s: &State) -> Result<UpdateGuard> {
    let _lock = s.0.mutations.lock().map_err(|_| "Application is busy")?;
    s.allow_work()?;
    if s.all::<Job>("job")?
        .iter()
        .any(|j| ["queued", "preparing", "running"].contains(&j.state.as_str()))
    {
        return Err("Finish or cancel queued downloads before installing an update.".into());
    }
    if !s
        .0
        .connection_tasks
        .lock()
        .map_err(|_| "Connection is busy")?
        .is_empty()
    {
        return Err("Wait for connection activity to finish before installing an update.".into());
    }
    s.0.updating.store(true, Ordering::SeqCst);
    Ok(UpdateGuard(s.clone()))
}
#[tauri::command]
pub async fn install_update(app: tauri::AppHandle, version: String) -> Result<()> {
    let s = app.state::<State>().inner().clone();
    let _guard = begin(&s)?;
    let update = app
        .updater_builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|_| "Cannot initialize the updater")?
        .check()
        .await
        .map_err(|_| "Could not contact the update service. Please try again.")?
        .ok_or("No newer update is available")?;
    if update.version != version {
        return Err("The available version changed. Check for updates again.".into());
    }
    let mut downloaded = 0u64;
    let bytes = update.download(|chunk, total| {
        downloaded += chunk as u64;
        let _ = app.emit("app-update-progress", serde_json::json!({"downloaded":downloaded,"total":total}));
    }, || {}).await.map_err(|_| "Update download or signature verification failed. Nothing was installed. Please try again.")?;
    let _ = app.emit("app-update-installing", ());
    // Installation is blocking; keep the event loop responsive. Windows exits from the installer.
    tauri::async_runtime::spawn_blocking(move || {
        update.install(bytes).map_err(|_| "Could not install the update. Quit Remote Desk, reopen it from Applications or the Start menu, and try again.")?;
        // LaunchServices may start the replacement before this process has exited.
        // Release the single-instance lock only after a successful install and worker shutdown.
        #[cfg(target_os = "macos")]
        {
            s.0.stop.store(true, Ordering::SeqCst);
            if let Some(worker) = s.0.worker.lock().unwrap().take() { let _ = worker.join(); }
            fs2::FileExt::unlock(&s.0._lock).map_err(|_| "Update installed; quit and reopen Remote Desk to finish.")?;
        }
        app.restart();
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    }).await.map_err(|_| "Update installation could not finish")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn update_gate_blocks_work_and_resets_on_failure() {
        let path =
            std::env::temp_dir().join(format!("remote-desk-update-{}", uuid::Uuid::new_v4()));
        {
            let s = State::open(path.clone()).unwrap();
            let gate = begin(&s).unwrap();
            assert!(s.allow_work().is_err());
            assert!(begin(&s).is_err());
            drop(gate);
            assert!(s.allow_work().is_ok());
            let mut j = Job {
                id: "fixture".into(),
                remote_id: "fixture".into(),
                path: "file".into(),
                destination: "fixture".into(),
                state: "queued".into(),
                detail: String::new(),
                stats: serde_json::json!({}),
            };
            for state in ["queued", "preparing", "running"] {
                j.state = state.into();
                s.put("job", &j.id, &j).unwrap();
                assert!(begin(&s).is_err());
                assert!(s.allow_work().is_ok());
            }
            j.state = "completed".into();
            s.put("job", &j.id, &j).unwrap();
            s.0.connection_tasks
                .lock()
                .unwrap()
                .insert("fixture".into(), 1);
            assert!(begin(&s).is_err());
            s.0.connection_tasks.lock().unwrap().clear();
            assert!(begin(&s).is_ok());
        }
        std::fs::remove_dir_all(path).unwrap();
    }
}
