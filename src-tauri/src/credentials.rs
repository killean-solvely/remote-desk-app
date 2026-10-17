//! Passwords remain in native UI/Rust memory; IPC returns only saved/canceled.
use zeroize::Zeroizing;

pub fn delete(id: &str) -> Result<(), String> {
    let entry = keyring::Entry::new("com.remotedesk.app", id)
        .map_err(|_| "Cannot open OS credential store")?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("Cannot remove the saved password from the OS credential store".into()),
    }
}

#[cfg(windows)]
pub fn prompt(user: &str, host: &str, parent: isize) -> Result<Option<Zeroizing<String>>, String> {
    use windows_sys::Win32::Security::Credentials::*;
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let title = wide("Remote Desk — SFTP password");
    let message = wide(&format!(
        "Password for {user}@{host}. Saved in Windows Credential Manager."
    ));
    let target = wide("Remote Desk SFTP");
    let mut username = vec![0u16; 514];
    let initial = wide(user);
    if initial.len() > username.len() {
        return Err("Username is too long for the native prompt".into());
    }
    username[..initial.len()].copy_from_slice(&initial);
    let mut password = Zeroizing::new(vec![0u16; 2561]);
    let info = CREDUI_INFOW {
        cbSize: std::mem::size_of::<CREDUI_INFOW>() as u32,
        hwndParent: parent as _,
        pszMessageText: message.as_ptr(),
        pszCaptionText: title.as_ptr(),
        hbmBanner: std::ptr::null_mut(),
    };
    let mut save = 0;
    // All pointers reference live, bounded buffers until this synchronous call returns.
    let result = unsafe {
        CredUIPromptForCredentialsW(
            &info,
            target.as_ptr(),
            std::ptr::null(),
            0,
            username.as_mut_ptr(),
            username.len() as u32,
            password.as_mut_ptr(),
            password.len() as u32,
            &mut save,
            CREDUI_FLAGS_GENERIC_CREDENTIALS
                | CREDUI_FLAGS_ALWAYS_SHOW_UI
                | CREDUI_FLAGS_DO_NOT_PERSIST
                | CREDUI_FLAGS_KEEP_USERNAME,
        )
    };
    if result == 1223 {
        return Ok(None);
    }
    if result != 0 {
        return Err("Could not open the native password dialog".into());
    }
    let username_end = username
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(username.len());
    if String::from_utf16_lossy(&username[..username_end]) != user {
        return Err(
            "The username changed in the native dialog; edit the saved connection first".into(),
        );
    }
    let end = password
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(password.len());
    let value = String::from_utf16(&password[..end]).map_err(|_| "Invalid password encoding")?;
    Ok(Some(Zeroizing::new(value)))
}

#[cfg(target_os = "macos")]
pub fn prompt(user: &str, host: &str, _parent: isize) -> Result<Option<Zeroizing<String>>, String> {
    use std::process::{Command, Stdio};
    // Only the public connection identity is an argument. Static AppleScript
    // prompts via macOS Standard Additions; no shell or interpolated script.
    let script = include_str!("password_prompt.applescript");
    let output = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            script,
            "--",
            &format!("Password for {user}@{host}. Saved in macOS Keychain."),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "Could not open the native password dialog")?;
    let bytes = Zeroizing::new(output.stdout);
    if !output.status.success() {
        return Err("Native password dialog failed; no password was changed".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "Invalid password encoding")?;
    if text.trim_end_matches(['\r', '\n']) == "canceled" {
        return Ok(None);
    }
    let password = text
        .strip_prefix("saved:")
        .and_then(|s| s.strip_suffix('\n'))
        .ok_or("Invalid native dialog response")?;
    Ok(Some(Zeroizing::new(password.to_owned())))
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn prompt(
    _user: &str,
    _host: &str,
    _parent: isize,
) -> Result<Option<Zeroizing<String>>, String> {
    Err("Native password entry is available on Windows and macOS".into())
}
