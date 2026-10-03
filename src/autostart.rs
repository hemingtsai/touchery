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
        // Persist the disable first. Removing the plist is what stops the
        // agent from being loaded at the next login; `launchctl unload` would
        // terminate this very process (launchctl's documented behaviour) and
        // could kill us before the file was ever deleted, leaving autostart
        // in place. The already-running instance is meant to keep running —
        // that is all a "launch at login" switch changes.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
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

    // Same ordering rule as above: make the persistent state correct before
    // running launchctl, so an unload that stops this process cannot lose it.
    std::fs::write(&path, xml)?;

    // Drop any previously loaded copy before loading the new file.
    let _ = Command::new("launchctl")
        .args(["unload", &path.display().to_string()])
        .output();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_path_lives_under_the_user_launch_agents_folder() {
        let path = plist_path().expect("tests need a home directory");
        assert_eq!(path.file_name().unwrap(), "com.touchery.app.plist");
        assert!(path.to_string_lossy().contains("/Library/LaunchAgents/"));
    }

    #[test]
    fn executable_paths_are_xml_escaped() {
        let escaped = xml_escape("/Applications/A&B <Utilities>/Touchery\"s 'app'");
        assert_eq!(
            escaped,
            "/Applications/A&amp;B &lt;Utilities&gt;/Touchery&quot;s &apos;app&apos;"
        );
        // The escaped value must not introduce any raw XML delimiter.
        assert!(
            !escaped.contains(['<', '>', '"', '\'']),
            "raw XML delimiters must not survive: {escaped}"
        );
    }
}
