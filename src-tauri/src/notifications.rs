mod platform;
use crate::{model::Job, Result, State};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use tauri::{Emitter, Manager};

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    NotRequested,
    FirstUse,
    Granted,
    Denied,
    Unavailable,
}
#[derive(Clone, Debug, Serialize)]
pub struct Access {
    permission: Permission,
    detail: String,
}
impl Access {
    fn new(permission: Permission, detail: &str) -> Self {
        Self {
            permission,
            detail: detail.into(),
        }
    }
}
#[derive(Default, Serialize, Deserialize)]
struct Preference {
    enabled: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    enabled: bool,
    permission: Permission,
    detail: String,
    last_error: Option<String>,
    test_pending: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Notice {
    Completed,
    Failed,
    Test,
}
impl Notice {
    pub fn title(self) -> &'static str {
        match self {
            Self::Completed => "Download completed",
            Self::Failed => "Download failed",
            Self::Test => "Remote Desk test",
        }
    }
    pub fn body(self) -> &'static str {
        match self {
            Self::Test => "Desktop notifications are working. Download alerts will appear here.",
            _ => "Open Remote Desk to view transfer history.",
        }
    }
}
#[derive(Default)]
struct Attempts {
    live: HashSet<String>,
}
impl Attempts {
    fn begin(&mut self, id: &str) {
        self.live.insert(id.into());
    }
    fn finish(&mut self, id: &str, state: &str, committed: bool) -> Option<Notice> {
        if !["completed", "failed", "canceled", "interrupted"].contains(&state) {
            return None;
        }
        // Consume even suppressed outcomes: focusing/enabling later cannot replay them.
        if !self.live.remove(id) || !committed {
            return None;
        }
        match state {
            "completed" => Some(Notice::Completed),
            "failed" => Some(Notice::Failed),
            _ => None,
        }
    }
}
#[derive(Default)]
pub struct Runtime {
    attempts: Mutex<Attempts>,
    last_error: Mutex<Option<String>>,
    configuring: AtomicBool,
    testing: AtomicBool,
}
struct Busy<'a>(&'a AtomicBool);
impl<'a> Busy<'a> {
    fn enter(flag: &'a AtomicBool) -> Result<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "A notification request is already in progress")?;
        Ok(Self(flag))
    }
}
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
fn preference(s: &State) -> Result<Preference> {
    Ok(s.all::<Preference>("notification_settings")?
        .into_iter()
        .next()
        .unwrap_or_default())
}
fn remember(s: &State, error: Option<String>) {
    if let Ok(mut last) = s.0.notifications.last_error.lock() {
        *last = error;
    }
}
fn probe(app: &tauri::AppHandle, request: bool) -> Access {
    // Do not borrow Terminal/PowerShell's notification identity in development.
    if tauri::is_dev() {
        return Access::new(
            Permission::Unavailable,
            "Use the installed Remote Desk app to enable and test desktop notifications.",
        );
    }
    platform::permission(&app.config().identifier, request)
        .unwrap_or_else(|e| Access::new(Permission::Unavailable, &e))
}
fn status(s: &State, access: Access) -> Result<Status> {
    Ok(Status {
        enabled: preference(s)?.enabled,
        permission: access.permission,
        detail: access.detail,
        last_error: s
            .0
            .notifications
            .last_error
            .lock()
            .ok()
            .and_then(|e| e.clone()),
        test_pending: s.0.notifications.testing.load(Ordering::SeqCst),
    })
}
fn current(s: &State, app: &tauri::AppHandle) -> Result<Status> {
    let access = if preference(s)?.enabled {
        probe(app, false)
    } else {
        Access::new(
            Permission::NotRequested,
            "Desktop notifications are off. Enable them to allow background download alerts.",
        )
    };
    status(s, access)
}
fn background(app: &tauri::AppHandle) -> bool {
    !app.webview_windows().is_empty() && platform::is_background().unwrap_or(false)
}
fn eligible(enabled: bool, background: bool, stopping: bool, permission: Permission) -> bool {
    enabled
        && background
        && !stopping
        && matches!(permission, Permission::Granted | Permission::FirstUse)
}
fn deliver(s: &State, app: &tauri::AppHandle, notice: Notice) -> Result<bool> {
    let enabled = preference(s)?.enabled;
    if !enabled || !background(app) || s.0.stop.load(Ordering::SeqCst) {
        return Ok(false);
    }
    let access = probe(app, false);
    if !eligible(
        enabled,
        background(app),
        s.0.stop.load(Ordering::SeqCst),
        access.permission,
    ) {
        return Ok(false);
    }
    platform::send(&app.config().identifier, notice)?;
    Ok(true)
}
pub fn begin(s: &State, id: &str) {
    if let Ok(mut attempts) = s.0.notifications.attempts.lock() {
        attempts.begin(id);
    }
}
pub fn finished(s: &State, app: &tauri::AppHandle, job: &Job, committed: bool) {
    let notice =
        s.0.notifications
            .attempts
            .lock()
            .ok()
            .and_then(|mut a| a.finish(&job.id, &job.state, committed));
    if let Some(notice) = notice {
        if !preference(s).is_ok_and(|p| p.enabled)
            || !background(app)
            || s.0.stop.load(Ordering::SeqCst)
        {
            return;
        }
        let s = s.clone();
        let app = app.clone();
        // OS delivery cannot delay the queue or change the committed job result.
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(e) = deliver(&s, &app, notice) {
                remember(&s, Some(e));
                let _ = app.emit("notification_status_changed", ());
            }
        });
    }
}
#[tauri::command]
pub async fn get_notification_status(
    s: tauri::State<'_, State>,
    app: tauri::AppHandle,
) -> Result<Status> {
    let s = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || current(&s, &app))
        .await
        .map_err(|_| "Could not check notification settings")?
}
#[tauri::command]
pub async fn set_notifications(
    s: tauri::State<'_, State>,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<Status> {
    let s = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = Busy::enter(&s.0.notifications.configuring)?;
        s.allow_work()?;
        // Only this explicit enable action may request OS permission. Hold no
        // download/database mutation lock while the permission dialog is open.
        let access = if enabled {
            probe(&app, true)
        } else {
            Access::new(Permission::NotRequested, "Desktop notifications are off.")
        };
        let _guard = s.0.mutations.lock().map_err(|_| "Settings are busy")?;
        s.allow_work()?;
        s.put("notification_settings", "main", &Preference { enabled })?;
        remember(&s, None);
        status(&s, access)
    })
    .await
    .map_err(|_| "Could not change notification settings")?
}
#[tauri::command]
pub async fn test_notification(
    s: tauri::State<'_, State>,
    app: tauri::AppHandle,
) -> Result<Status> {
    let s = s.inner().clone();
    s.allow_work()?;
    if !preference(&s)?.enabled {
        return Err("Enable desktop notifications before testing".into());
    }
    let initial = {
        let s = s.clone();
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || current(&s, &app))
            .await
            .map_err(|_| "Could not check notification permission")??
    };
    if !matches!(
        initial.permission,
        Permission::Granted | Permission::FirstUse
    ) {
        return Err(initial.detail);
    }
    s.0.notifications
        .testing
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| "A test notification is already scheduled")?;
    remember(&s, None);
    let task_s = s.clone();
    let task_app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let _=tauri::async_runtime::spawn_blocking(move||{
            let _busy=Busy(&task_s.0.notifications.testing);
            match deliver(&task_s,&task_app,Notice::Test) {
                Ok(true)=>remember(&task_s,None),
                Ok(false) if preference(&task_s).is_ok_and(|p|p.enabled) && !task_s.0.stop.load(Ordering::SeqCst)=>remember(&task_s,Some("Test not shown. Switch to another app and check notification permission, then try again.".into())),
                Ok(false)=>{},Err(e)=>remember(&task_s,Some(e)),
            }
            drop(_busy);
            let _=task_app.emit("notification_status_changed",());
        }).await;
    });
    status(&s, Access::new(initial.permission, &initial.detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_committed_live_outcomes_notify_once_per_attempt() {
        let mut a = Attempts::default();
        assert_eq!(a.finish("old history", "completed", true), None);
        a.begin("job");
        for state in ["queued", "preparing", "running"] {
            assert_eq!(a.finish("job", state, true), None);
        }
        assert_eq!(a.finish("job", "completed", true), Some(Notice::Completed));
        assert_eq!(a.finish("job", "completed", true), None);
        a.begin("job");
        assert_eq!(a.finish("job", "failed", true), Some(Notice::Failed));
        for state in ["canceled", "interrupted"] {
            a.begin("job");
            assert_eq!(a.finish("job", state, true), None);
        }
        a.begin("job");
        assert_eq!(a.finish("job", "failed", false), None);
        assert_eq!(a.finish("job", "failed", true), None);
    }
    #[test]
    fn suppressed_outcomes_never_replay_and_messages_are_generic() {
        for (enabled, bg, stop, permission) in [
            (false, true, false, Permission::Granted),
            (true, false, false, Permission::Granted),
            (true, true, true, Permission::Granted),
            (true, true, false, Permission::Denied),
            (true, true, false, Permission::NotRequested),
            (true, true, false, Permission::Unavailable),
        ] {
            assert!(!eligible(enabled, bg, stop, permission));
        }
        assert!(eligible(true, true, false, Permission::Granted));
        assert!(eligible(true, true, false, Permission::FirstUse));
        let mut a = Attempts::default();
        a.begin("foreground-job");
        a.finish("foreground-job", "completed", true);
        assert_eq!(a.finish("foreground-job", "completed", true), None);
        assert_eq!(Notice::Completed.title(), "Download completed");
        assert_eq!(Notice::Failed.title(), "Download failed");
        for n in [Notice::Completed, Notice::Failed, Notice::Test] {
            assert!(!n.body().contains('/'));
            assert!(!n.body().contains('@'));
        }
    }
    #[test]
    fn preference_is_opt_in_persistent_and_separate_from_transfer_settings() {
        let data = std::env::temp_dir().join(format!(
            "remote-desk-notifications-{}",
            uuid::Uuid::new_v4()
        ));
        {
            let s = State::open(data.clone()).unwrap();
            assert!(!preference(&s).unwrap().enabled);
            s.put(
                "notification_settings",
                "main",
                &Preference { enabled: true },
            )
            .unwrap();
            s.put("settings", "main", &crate::model::Settings::default())
                .unwrap();
            assert!(preference(&s).unwrap().enabled);
        }
        {
            let s = State::open(data.clone()).unwrap();
            assert!(preference(&s).unwrap().enabled);
            assert!(s.0.notifications.attempts.lock().unwrap().live.is_empty());
            s.put(
                "notification_settings",
                "main",
                &Preference { enabled: false },
            )
            .unwrap();
            assert!(!preference(&s).unwrap().enabled);
        }
        std::fs::remove_dir_all(data).unwrap();
    }
}
