//! Login-item management via a per-user LaunchAgent.
//!
//! We don't use SMAppService/SMLoginItemSetEnabled because both require a
//! properly provisioned app bundle identity; a LaunchAgent plist works for
//! ad-hoc-signed builds and points directly at the current executable.

use std::path::PathBuf;
use std::process::Command;

const LABEL: &str = "com.touchery.app";

fn plist_path() -> Option<PathBuf> {
    dirs::home_dir()
        .map(|home| home.join("Library").join("LaunchAgents").join(format!("{LABEL}.plist")))
}

/// Whether the LaunchAgent currently exists on disk.
pub fn is_enabled() -> bool {
    plist_path().is_some_and(|p| p.exists())
}

/// Enable/disable launch-at-login by installing/removing the LaunchAgent.
pub fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    let Some(path) = plist_path() else {
        anyhow::bail!("no home directory available");
    };

    if !enabled {
        let _ = Command::new("launchctl").args(["unload", &path.display().to_string()]).output();
        std::fs::remove_file(&path)?;
        return Ok(());
    }

    let exe = std::env::current_exe()?;
    let exe_str = exe.display().to_string();
    let escaped = xml_escape(&exe_str);
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <!-- Menu bar app: keep running, but never relaunch after manual quit. -->
    <key>KeepAlive</key>
    <false/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        escaped
    );

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    // Unload old agent before writing new plist to avoid stale content.
    let _ = Command::new("launchctl")
        .args(["unload", &path.display().to_string()])
        .output();

    std::fs::write(&path, xml)?;

    // Load the new agent.
    let out = Command::new("launchctl")
        .args(["load", &path.display().to_string()])
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("launchctl load failed: {}", stderr.trim());
    }
    Ok(())
}

/// Escape XML special characters to prevent malformed plist or injection.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
