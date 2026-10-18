use super::{Access, Notice, Permission};
use crate::Result;

#[cfg(windows)]
mod native {
    use super::*;
    use windows::{
        core::HSTRING,
        Data::Xml::Dom::XmlDocument,
        Win32::{
            Foundation::RPC_E_CHANGED_MODE,
            System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
        },
        UI::Notifications::{
            NotificationSetting, ToastNotification, ToastNotificationManager, ToastNotifier,
        },
    };
    struct Apartment(bool);
    pub fn is_background() -> Result<bool> {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        let window = unsafe { GetForegroundWindow() };
        if window.0.is_null() {
            return Err("Cannot determine the focused application".into());
        }
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(window, Some(&mut pid));
        }
        if pid == 0 {
            return Err("Cannot determine the focused application".into());
        }
        Ok(pid != std::process::id())
    }
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    RoUninitialize();
                }
            }
        }
    }
    fn with_notifier<T>(id: &str, f: impl FnOnce(&ToastNotifier) -> Result<T>) -> Result<T> {
        let initialized = match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            Ok(()) => true,
            Err(e) if e.code() == RPC_E_CHANGED_MODE => false,
            Err(_) => return Err("Windows could not initialize desktop notifications.".into()),
        };
        let _apartment = Apartment(initialized);
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(id))
            .map_err(|_| {
                "Windows could not find Remote Desk's notification identity. Use the installed app."
            })?;
        f(&notifier)
    }
    pub fn permission(id: &str, _request: bool) -> Result<Access> {
        with_notifier(id, |notifier| classify(notifier.Setting()))
    }
    fn classify(value: windows::core::Result<NotificationSetting>) -> Result<Access> {
        let setting = match value {
                Ok(setting)=>setting,
                // Windows creates this per-app preference entry with the first
                // toast. Do not mistake an installed app's first use for denial.
                Err(e) if e.code().0 as u32==0x80070490=>return Ok(Access::new(Permission::FirstUse,"Windows initializes this app's notification preferences with its first alert. Send a test, then check permission. Windows settings and Focus still apply.")),
                Err(_)=>return Err("Could not check Windows notification permission.".into()),
            };
        Ok(match setting {
                NotificationSetting::Enabled=>Access::new(Permission::Granted,"Allowed by Windows. Focus and system notification settings can still silence alerts."),
                NotificationSetting::DisabledByGroupPolicy=>Access::new(Permission::Denied,"Notifications are blocked by Windows policy. Contact your administrator."),
                NotificationSetting::DisabledByManifest=>Access::new(Permission::Unavailable,"Windows cannot register notifications for this copy. Reinstall Remote Desk using its installer."),
                NotificationSetting::DisabledForApplication|NotificationSetting::DisabledForUser=>Access::new(Permission::Denied,"Notifications are blocked. Open Windows Settings > System > Notifications and allow Remote Desk."),
                _=>Access::new(Permission::Unavailable,"Windows notification permission could not be determined."),
            })
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn first_use_is_not_a_denial_and_real_denials_are_preserved() {
            use windows::core::{Error, HRESULT};
            let unknown_app = Error::from_hresult(HRESULT(0x80070490u32 as i32));
            assert_eq!(
                classify(Err(unknown_app)).unwrap().permission,
                Permission::FirstUse
            );
            for setting in [
                NotificationSetting::DisabledForApplication,
                NotificationSetting::DisabledForUser,
                NotificationSetting::DisabledByGroupPolicy,
            ] {
                assert_eq!(
                    classify(Ok(setting)).unwrap().permission,
                    Permission::Denied
                );
            }
            assert_eq!(
                classify(Ok(NotificationSetting::Enabled))
                    .unwrap()
                    .permission,
                Permission::Granted
            );
            assert!(classify(Err(Error::from_hresult(HRESULT(0x80070005u32 as i32)))).is_err());
        }
    }
    pub fn send(id: &str, notice: Notice) -> Result<()> {
        with_notifier(id, |notifier| {
            // Only fixed, application-owned text enters this XML; no remote or
            // local path, username, job detail or frontend payload is accepted.
            let xml=format!("<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",notice.title(),notice.body());
            let doc =
                XmlDocument::new().map_err(|_| "Could not prepare a Windows notification.")?;
            doc.LoadXml(&HSTRING::from(xml))
                .map_err(|_| "Could not prepare a Windows notification.")?;
            let toast = ToastNotification::CreateToastNotification(&doc)
                .map_err(|_| "Could not prepare a Windows notification.")?;
            let tag = uuid::Uuid::new_v4().simple().to_string();
            toast
                .SetTag(&HSTRING::from(&tag[..16]))
                .map_err(|_| "Could not prepare a Windows notification.")?;
            notifier.Show(&toast).map_err(|_| {
                "Windows could not accept the notification. Check system notification settings."
                    .into()
            })
        })
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use block2::RcBlock;
    use objc2::{rc::Retained, runtime::Bool};
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
        UNNotificationRequest, UNNotificationSetting, UNNotificationSettings, UNNotificationSound,
        UNUserNotificationCenter,
    };
    use std::{ptr::NonNull, sync::mpsc, time::Duration};
    pub fn is_background() -> Result<bool> {
        objc2::rc::autoreleasepool(|_| {
            Ok(!objc2_app_kit::NSRunningApplication::currentApplication().isActive())
        })
    }
    fn center(id: &str) -> Result<Retained<UNUserNotificationCenter>> {
        if !NSBundle::mainBundle()
            .bundleIdentifier()
            .is_some_and(|bundle| bundle.to_string() == id)
        {
            return Err(
                "Use Remote Desk from Applications to enable notifications under its own identity."
                    .into(),
            );
        }
        Ok(UNUserNotificationCenter::currentNotificationCenter())
    }
    fn settings(center: &UNUserNotificationCenter) -> Result<Access> {
        let (tx, rx) = mpsc::channel();
        let callback = RcBlock::new(move |ptr: NonNull<UNNotificationSettings>| {
            // The OS guarantees the settings object is valid during this block.
            let s = unsafe { ptr.as_ref() };
            let access=match s.authorizationStatus() {
                UNAuthorizationStatus::NotDetermined=>Access::new(Permission::NotRequested,"macOS permission has not been requested. Enable notifications to ask for permission."),
                UNAuthorizationStatus::Denied=>Access::new(Permission::Denied,"Notifications are blocked. Open System Settings > Notifications > Remote Desk and allow notifications."),
                UNAuthorizationStatus::Authorized|UNAuthorizationStatus::Provisional|UNAuthorizationStatus::Ephemeral=>{
                    if s.alertSetting()!=UNNotificationSetting::Enabled {
                        if s.notificationCenterSetting()==UNNotificationSetting::Enabled { Access::new(Permission::Granted,"Allowed, but banners are off. Check Notification Center or enable banners in System Settings > Notifications > Remote Desk.") }
                        else { Access::new(Permission::Denied,"Alerts are disabled. Enable banners or Notification Center in System Settings > Notifications > Remote Desk.") }
                    } else { Access::new(Permission::Granted,"Allowed by macOS. Focus and system notification settings can still silence alerts.") }
                }
                _=>Access::new(Permission::Unavailable,"macOS notification permission could not be determined."),
            };
            let _ = tx.send(access);
        });
        center.getNotificationSettingsWithCompletionHandler(&callback);
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| {
            "macOS did not return notification settings. Try checking permission again.".into()
        })
    }
    pub fn permission(id: &str, request: bool) -> Result<Access> {
        objc2::rc::autoreleasepool(|_| permission_inner(id, request))
    }
    fn permission_inner(id: &str, request: bool) -> Result<Access> {
        let center = center(id)?;
        let access = settings(&center)?;
        if request && access.permission == Permission::NotRequested {
            let (tx, rx) = mpsc::channel();
            let callback = RcBlock::new(move |_allowed: Bool, error: *mut NSError| {
                let _ = tx.send(error.is_null());
            });
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &callback,
            );
            if !rx.recv_timeout(Duration::from_secs(120)).map_err(|_|"The macOS permission request is still pending. Check permission again after responding.")? {
                return Err("macOS could not request notification permission. Check System Settings > Notifications.".into());
            }
            settings(&center)
        } else {
            Ok(access)
        }
    }
    pub fn send(id: &str, notice: Notice) -> Result<()> {
        objc2::rc::autoreleasepool(|_| send_inner(id, notice))
    }
    fn send_inner(id: &str, notice: Notice) -> Result<()> {
        let center = center(id)?;
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(notice.title()));
        content.setBody(&NSString::from_str(notice.body()));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let identifier = NSString::from_str(&uuid::Uuid::new_v4().to_string());
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &identifier,
            &content,
            None,
        );
        let (tx, rx) = mpsc::channel();
        let callback = RcBlock::new(move |error: *mut NSError| {
            let _ = tx.send(error.is_null());
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&callback));
        if rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "macOS did not acknowledge the notification request.")?
        {
            Ok(())
        } else {
            Err("macOS could not accept the notification. Check System Settings > Notifications > Remote Desk.".into())
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub use native::{is_background, permission, send};
#[cfg(not(any(windows, target_os = "macos")))]
pub fn permission(_: &str, _: bool) -> Result<Access> {
    Ok(Access::new(
        Permission::Unavailable,
        "Desktop notifications are supported on Windows and macOS.",
    ))
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn send(_: &str, _: Notice) -> Result<()> {
    Err("Desktop notifications are unavailable on this platform.".into())
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn is_background() -> Result<bool> {
    Ok(false)
}
