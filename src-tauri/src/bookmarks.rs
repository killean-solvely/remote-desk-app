use crate::{engine, model::*, ConnectionTask, Result, State};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub id: String,
    pub remote_id: String,
    pub label: String,
    pub path: String,
    pub needs_review: bool,
    pub revision: String,
}

fn label(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 120 || value.chars().any(char::is_control) {
        return Err("Use a bookmark name of 1–120 characters without control characters".into());
    }
    Ok(value.into())
}
fn find(s: &State, id: &str) -> Result<Bookmark> {
    s.all::<Bookmark>("bookmark")?
        .into_iter()
        .find(|b| b.id == id)
        .ok_or("Bookmark not found".into())
}
fn add(s: &State, remote_id: &str, path: &str, name: &str) -> Result<Bookmark> {
    let _guard = s.0.mutations.lock().map_err(|_| "Bookmarks are busy")?;
    s.allow_work()?;
    s.remote(remote_id)?;
    remote_relative(path)?;
    let name = label(name)?;
    if let Some(existing) = s
        .all::<Bookmark>("bookmark")?
        .into_iter()
        .find(|b| b.remote_id == remote_id && b.path == path)
    {
        return Ok(existing);
    }
    let b = Bookmark {
        id: uuid::Uuid::new_v4().to_string(),
        remote_id: remote_id.into(),
        label: name,
        path: path.into(),
        needs_review: false,
        revision: uuid::Uuid::new_v4().to_string(),
    };
    s.put("bookmark", &b.id, &b)?;
    Ok(b)
}
fn rename(s: &State, id: &str, name: &str) -> Result<Bookmark> {
    let _guard = s.0.mutations.lock().map_err(|_| "Bookmarks are busy")?;
    s.allow_work()?;
    let mut b = find(s, id)?;
    b.label = label(name)?;
    s.put("bookmark", &b.id, &b)?;
    Ok(b)
}
fn remove(s: &State, id: &str) -> Result<()> {
    let _guard = s.0.mutations.lock().map_err(|_| "Bookmarks are busy")?;
    s.allow_work()?;
    find(s, id)?;
    s.0.db
        .lock()
        .map_err(|_| "Database lock failed")?
        .execute("DELETE FROM records WHERE kind='bookmark' AND id=?1", [id])
        .map_err(|_| "Cannot remove bookmark")?;
    Ok(())
}
fn confirm(s: &State, id: &str, revision: &str) -> Result<Bookmark> {
    let _guard = s.0.mutations.lock().map_err(|_| "Bookmarks are busy")?;
    s.allow_work()?;
    let mut b = find(s, id)?;
    s.remote(&b.remote_id)?;
    remote_relative(&b.path)?;
    if b.revision != revision {
        return Err(
            "The connection changed again. Review the updated bookmark before confirming.".into(),
        );
    }
    b.needs_review = false;
    s.put("bookmark", &b.id, &b)?;
    Ok(b)
}
fn lease(s: &State, id: &str) -> Result<(Bookmark, Remote, ConnectionTask)> {
    let _guard = s.0.mutations.lock().map_err(|_| "Connection is busy")?;
    s.allow_work()?;
    let b = find(s, id)?;
    if b.needs_review {
        return Err(
            "This connection changed. Review the bookmark target before opening it.".into(),
        );
    }
    remote_relative(&b.path)?;
    let remote = s.remote(&b.remote_id)?;
    let task = s.connection_task(&remote.id)?;
    Ok((b, remote, task))
}

// Called under the shared mutation gate: connection identity and bookmark
// invalidation commit together, including edits back to an earlier identity.
pub fn persist_connection(s: &State, old: &Remote, remote: &Remote) -> Result<()> {
    let changed = old.host != remote.host
        || old.port != remote.port
        || old.user != remote.user
        || old.root != remote.root;
    let bookmarks = if changed {
        s.all::<Bookmark>("bookmark")?
    } else {
        vec![]
    };
    let mut db = s.0.db.lock().map_err(|_| "Database lock failed")?;
    let tx = db.transaction().map_err(|_| "Cannot update connection")?;
    tx.execute(
        "INSERT INTO records(kind,id,data) VALUES('remote',?1,?2) ON CONFLICT(kind,id) DO UPDATE SET data=excluded.data",
        params![
            remote.id,
            serde_json::to_string(remote).map_err(|_| "Cannot encode connection")?
        ],
    )
    .map_err(|_| "Cannot save connection")?;
    for mut b in bookmarks.into_iter().filter(|b| b.remote_id == remote.id) {
        b.needs_review = true;
        b.revision = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "UPDATE records SET data=?1 WHERE kind='bookmark' AND id=?2",
            params![
                serde_json::to_string(&b).map_err(|_| "Cannot encode bookmark")?,
                b.id
            ],
        )
        .map_err(|_| "Cannot update bookmarks")?;
    }
    tx.commit()
        .map_err(|_| "Cannot save connection and bookmarks".into())
}
pub fn delete_connection_records(s: &State, remote_id: &str) -> Result<()> {
    let bookmarks = s.all::<Bookmark>("bookmark")?;
    let mut db = s.0.db.lock().map_err(|_| "Database lock failed")?;
    let tx = db.transaction().map_err(|_| "Cannot delete connection")?;
    for b in bookmarks.into_iter().filter(|b| b.remote_id == remote_id) {
        tx.execute(
            "DELETE FROM records WHERE kind='bookmark' AND id=?1",
            [b.id],
        )
        .map_err(|_| "Cannot remove connection bookmarks")?;
    }
    tx.execute(
        "DELETE FROM records WHERE kind='remote' AND id=?1",
        [remote_id],
    )
    .map_err(|_| "Cannot delete connection")?;
    tx.commit()
        .map_err(|_| "Cannot save connection deletion".into())
}

#[tauri::command]
pub fn list_bookmarks(s: tauri::State<State>) -> Result<Vec<Bookmark>> {
    s.all("bookmark")
}
#[tauri::command]
pub fn add_bookmark(
    s: tauri::State<State>,
    remote_id: String,
    path: String,
    label: String,
) -> Result<Bookmark> {
    add(&s, &remote_id, &path, &label)
}
#[tauri::command]
pub fn rename_bookmark(s: tauri::State<State>, id: String, label: String) -> Result<Bookmark> {
    rename(&s, &id, &label)
}
#[tauri::command]
pub fn delete_bookmark(s: tauri::State<State>, id: String) -> Result<()> {
    remove(&s, &id)
}
#[tauri::command]
pub fn confirm_bookmark(s: tauri::State<State>, id: String, revision: String) -> Result<Bookmark> {
    confirm(&s, &id, &revision)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedBookmark {
    remote_id: String,
    path: String,
    entries: Vec<Entry>,
}
#[tauri::command]
pub async fn open_bookmark(s: tauri::State<'_, State>, id: String) -> Result<OpenedBookmark> {
    let s = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (b, remote, _task) = lease(&s, &id)?;
        let entries = engine::list(
            &s.settings()?,
            &remote,
            &s.hosts(&remote),
            &b.path,
            false,
            None,
        )?;
        Ok(OpenedBookmark {
            remote_id: remote.id,
            path: b.path,
            entries,
        })
    })
    .await
    .map_err(|_| "Bookmark browsing failed")?
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        data: std::path::PathBuf,
        state: Option<State>,
    }
    impl Fixture {
        fn new() -> Self {
            let data =
                std::env::temp_dir().join(format!("remotedesk-bookmarks-{}", uuid::Uuid::new_v4()));
            let state = State::open(data.clone()).unwrap();
            for id in ["one", "two"] {
                state
                    .put(
                        "remote",
                        id,
                        &Remote {
                            id: id.into(),
                            label: id.into(),
                            host: "offline.invalid".into(),
                            port: 22,
                            user: "test".into(),
                            root: String::new(),
                            auth: "agent".into(),
                            known_hosts: format!("{id}.known_hosts"),
                        },
                    )
                    .unwrap();
            }
            Self {
                data,
                state: Some(state),
            }
        }
        fn s(&self) -> &State {
            self.state.as_ref().unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.state.take();
            let _ = std::fs::remove_dir_all(&self.data);
        }
    }
    #[test]
    fn exact_paths_duplicates_and_restart_are_local_and_persistent() {
        let mut f = Fixture::new();
        let root = add(f.s(), "one", "", "Root").unwrap();
        let a = add(f.s(), "one", "Parent:remote/Games", "Games").unwrap();
        assert_eq!(add(f.s(), "one", &a.path, "Duplicate").unwrap().id, a.id);
        for path in [
            "Parent:remote/games",
            "caf\u{e9}",
            "cafe\u{301}",
            "trailing ",
        ] {
            add(f.s(), "one", path, "Folder").unwrap();
        }
        add(f.s(), "two", &a.path, "Other server").unwrap();
        for path in ["../escape", "a/../b", "/absolute", "a//b", "a\0b"] {
            assert!(add(f.s(), "one", path, "Bad").is_err());
        }
        for name in ["", "  ", "a\nb", &"x".repeat(121)] {
            assert!(rename(f.s(), &a.id, name).is_err());
        }
        assert!(add(f.s(), "missing", "", "No connection").is_err());
        assert_eq!(rename(f.s(), &a.id, "  Favorites  ").unwrap().path, a.path);
        assert_eq!(find(f.s(), &a.id).unwrap().label, "Favorites");
        f.state.take();
        f.state = Some(State::open(f.data.clone()).unwrap());
        assert_eq!(find(f.s(), &root.id).unwrap().path, "");
        let (b, r, task) = lease(f.s(), &a.id).unwrap();
        assert_eq!(b.path, a.path);
        assert_eq!(r.id, "one");
        assert!(f.s().idle_remote("one").is_err());
        drop(task);
        assert!(f.s().idle_remote("one").is_ok());
        remove(f.s(), &a.id).unwrap();
        assert!(find(f.s(), &a.id).is_err());
        assert_eq!(f.s().all::<Bookmark>("bookmark").unwrap().len(), 6);
        assert!(lease(f.s(), "unknown").is_err());
    }
    #[test]
    fn identity_changes_require_current_review_and_deletion_is_scoped() {
        let mut f = Fixture::new();
        let a = add(f.s(), "one", "Games", "Games").unwrap();
        let other = add(f.s(), "two", "Games", "Games").unwrap();
        let old = f.s().remote("one").unwrap();
        let mut changed = old.clone();
        changed.label = "New label".into();
        changed.auth = "password".into();
        persist_connection(f.s(), &old, &changed).unwrap();
        assert!(!find(f.s(), &a.id).unwrap().needs_review);
        for field in ["host", "port", "user", "root"] {
            let old = f.s().remote("one").unwrap();
            let mut changed = old.clone();
            match field {
                "host" => changed.host.push('x'),
                "port" => changed.port += 1,
                "user" => changed.user.push('x'),
                _ => changed.root.push_str("/new"),
            }
            persist_connection(f.s(), &old, &changed).unwrap();
            let b = find(f.s(), &a.id).unwrap();
            assert!(b.needs_review);
            assert!(lease(f.s(), &a.id).is_err());
            assert!(confirm(f.s(), &a.id, &a.revision).is_err());
            assert!(
                add(f.s(), "one", "Games", "Duplicate")
                    .unwrap()
                    .needs_review
            );
            assert!(rename(f.s(), &a.id, "Renamed").unwrap().needs_review);
            confirm(f.s(), &a.id, &b.revision).unwrap();
            assert!(lease(f.s(), &a.id).is_ok());
            assert!(!find(f.s(), &other.id).unwrap().needs_review);
        }
        // A second identity edit invalidates an already displayed review form.
        let old = f.s().remote("one").unwrap();
        let mut changed = old.clone();
        changed.root.push_str("/again");
        persist_connection(f.s(), &old, &changed).unwrap();
        let stale = find(f.s(), &a.id).unwrap();
        persist_connection(f.s(), &changed, &old).unwrap();
        assert!(confirm(f.s(), &a.id, &stale.revision).is_err());
        f.state.take();
        f.state = Some(State::open(f.data.clone()).unwrap());
        assert!(find(f.s(), &a.id).unwrap().needs_review);
        delete_connection_records(f.s(), "one").unwrap();
        assert!(find(f.s(), &a.id).is_err());
        assert!(f.s().remote("one").is_err());
        assert_eq!(find(f.s(), &other.id).unwrap().path, "Games");
        assert!(f.s().remote("two").is_ok());
    }
    #[test]
    fn connection_edits_and_update_gate_protect_bookmark_navigation() {
        let f = Fixture::new();
        let b = add(f.s(), "one", "Games", "Games").unwrap();
        let mut remote = f.s().remote("one").unwrap();
        remote.root = "/new-root".into();
        let (_, _, task) = lease(f.s(), &b.id).unwrap();
        assert!(crate::update_connection(f.s(), remote.clone(), false, false, None).is_err());
        assert_eq!(f.s().remote("one").unwrap().root, "");
        drop(task);
        crate::update_connection(f.s(), remote, false, false, None).unwrap();
        let b = find(f.s(), &b.id).unwrap();
        assert!(b.needs_review);
        assert!(lease(f.s(), &b.id).is_err());
        f.s()
            .0
            .updating
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(add(f.s(), "one", "Another", "Another").is_err());
        assert!(rename(f.s(), &b.id, "Changed").is_err());
        assert!(remove(f.s(), &b.id).is_err());
        assert!(confirm(f.s(), &b.id, &b.revision).is_err());
        assert!(lease(f.s(), &b.id).is_err());
        f.s()
            .0
            .updating
            .store(false, std::sync::atomic::Ordering::SeqCst);
        confirm(f.s(), &b.id, &b.revision).unwrap();
        assert!(lease(f.s(), &b.id).is_ok());
    }
}
