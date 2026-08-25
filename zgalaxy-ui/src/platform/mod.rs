//! Platform adapter (spec §11.1/§18): OS-specific behavior lives here only.

#[cfg(target_os = "linux")]
mod linux;

pub fn send_notification(title: &str, body: &str) {
    #[cfg(target_os = "linux")]
    crate::platform::linux::send_notification(title, body);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (title, body);
    }
}

pub fn copy_to_clipboard(text: &str) {
    #[cfg(target_os = "linux")]
    crate::platform::linux::copy_to_clipboard(text);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = text;
    }
}

/// Register/unregister the app to start with the user session
/// (spec §17 "Start with system" preference).
pub fn set_autostart(enabled: bool) {
    #[cfg(target_os = "linux")]
    crate::platform::linux::set_autostart(enabled);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = enabled;
    }
}
