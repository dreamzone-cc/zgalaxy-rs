//! Linux platform behavior (spec §18). Best-effort, failure-tolerant: the
//! app must keep working on minimal desktops without a notification daemon
//! or clipboard helpers.

use std::process::{Command, Stdio};

pub fn send_notification(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .arg("-a")
        .arg("ZGALAXY")
        .arg(title)
        .arg(body)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Wayland sessions use wl-copy; X11 uses xclip/xsel. Spawned detached so
/// the UI never waits on an external process.
pub fn copy_to_clipboard(text: &str) {
    let text = text.to_string();
    std::thread::spawn(move || {
        let via_stdin = |cmd: &mut Command| {
            cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
            if let Ok(mut child) = cmd.spawn() {
                if let Some(stdin) = child.stdin.as_mut() {
                    use std::io::Write;
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
                true
            } else {
                false
            }
        };
        let mut c = Command::new("wl-copy");
        if via_stdin(c.arg("--type").arg("text/plain")) {
            return;
        }
        let mut c = Command::new("xclip");
        if via_stdin(c.arg("-selection").arg("clipboard").arg("-in")) {
            return;
        }
        let mut c = Command::new("xsel");
        let _ = via_stdin(c.arg("--clipboard").arg("--input"));
    });
}

/// XDG autostart: ~/.config/autostart/zgalaxy-ui.desktop (spec §17).
/// Enabled → desktop entry launching this exact binary; disabled → removed.
/// Startup behavior (window vs background) is decided by settings.json, not
/// baked into the entry, so the Preferences toggle stays the single source.
pub fn set_autostart(enabled: bool) {
    let Some(home) = std::env::var_os("HOME") else { return };
    let dir = std::path::Path::new(&home).join(".config/autostart");
    let entry = dir.join("zgalaxy-ui.desktop");
    if !enabled {
        let _ = std::fs::remove_file(&entry);
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(exe) = exe.to_str() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let content = format!(
        "[Desktop Entry]\nType=Application\nName=ZGALAXY\nComment=ZGALAXY network client\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe
    );
    let _ = std::fs::write(&entry, content);
}
