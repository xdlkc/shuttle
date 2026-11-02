use crate::{Result, config};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};
const LABEL: &str = "local.shuttle.worker";
const LEGACY_LABEL: &str = "local.agent-relay.worker";
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn system(args: &[String]) -> bool {
    Command::new("/bin/launchctl")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
fn check_link(link: &Path, targets: &[&Path]) -> Result<()> {
    if fs::symlink_metadata(link).is_ok()
        && !fs::read_link(link).is_ok_and(|p| targets.iter().any(|t| p == *t))
    {
        return Err("command path already exists; not overwritten".into());
    }
    Ok(())
}
fn link(target: &Path, path: &Path) -> Result<()> {
    fs::create_dir_all(path.parent().ok_or("parent missing")?)?;
    let tmp = path.with_extension("shuttle-tmp");
    if fs::symlink_metadata(&tmp).is_ok() {
        return Err("another installation may be running".into());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, &tmp)?;
    fs::rename(tmp, path)?;
    Ok(())
}
fn unit_text(exe: &Path, data: &Path, path: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>{LABEL}</string><key>ProgramArguments</key><array><string>{}</string><string>--data-dir</string><string>{}</string><string>listen</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer><key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>",
        xml(&exe.to_string_lossy()),
        xml(&data.to_string_lossy()),
        xml(path),
        xml(&data.join("service.log").to_string_lossy()),
        xml(&data.join("service-error.log").to_string_lossy())
    )
}
fn install_with(
    data: &Path,
    home: &Path,
    binary: &[u8],
    path: &str,
    domain: &str,
    run: impl Fn(&[String]) -> bool,
) -> Result<()> {
    let exe = data.join("bin/shuttle");
    let legacy_exe = data.join("bin/agent-relay");
    let command = home.join(".local/bin/shuttle");
    let legacy_command = home.join(".local/bin/agent-relay");
    check_link(&command, &[&exe])?;
    let legacy_link = fs::read_link(&legacy_command).ok();
    let update_legacy = legacy_link
        .as_ref()
        .is_some_and(|p| p == &legacy_exe || p == &exe);
    let units = home.join("Library/LaunchAgents");
    let unit = units.join(format!("{LABEL}.plist"));
    let legacy_unit = units.join(format!("{LEGACY_LABEL}.plist"));
    let previous_unit = fs::read(&unit).ok();
    let previous_exe = fs::read(&exe).ok();
    let previous_link = fs::read_link(&command).ok();
    let legacy_running = run(&["print".into(), format!("{domain}/{LEGACY_LABEL}")]);
    let current_running = run(&["print".into(), format!("{domain}/{LABEL}")]);
    config::write(&exe, binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700))?;
    }
    config::write(&unit, unit_text(&exe, data, path).as_bytes())?;
    let result = (|| -> Result<()> {
        for (label, running) in [(LEGACY_LABEL, legacy_running), (LABEL, current_running)] {
            if running {
                run(&["bootout".into(), format!("{domain}/{label}")]);
                for _ in 0..40 {
                    if !run(&["print".into(), format!("{domain}/{label}")]) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if run(&["print".into(), format!("{domain}/{label}")]) {
                    return Err("previous worker still running".into());
                }
            }
        }
        link(&exe, &command)?;
        if update_legacy {
            link(&exe, &legacy_command)?;
        }
        if !run(&[
            "bootstrap".into(),
            domain.into(),
            unit.to_string_lossy().into(),
        ]) {
            return Err("autostart failed".into());
        }
        // Remove the old login item only after the new job has started.
        if legacy_unit.exists() {
            fs::remove_file(&legacy_unit)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        run(&["bootout".into(), format!("{domain}/{LABEL}")]);
        if let Some(bytes) = previous_unit {
            config::write(&unit, &bytes)?;
        } else {
            let _ = fs::remove_file(&unit);
        }
        if let Some(bytes) = previous_exe {
            config::write(&exe, &bytes)?;
        } else {
            let _ = fs::remove_file(&exe);
        }
        let _ = fs::remove_file(&command);
        if let Some(target) = previous_link {
            link(&target, &command)?;
        }
        if update_legacy {
            link(legacy_link.as_ref().unwrap(), &legacy_command)?;
        }
        if legacy_running {
            run(&[
                "bootstrap".into(),
                domain.into(),
                legacy_unit.to_string_lossy().into(),
            ]);
        } else if current_running {
            run(&[
                "bootstrap".into(),
                domain.into(),
                unit.to_string_lossy().into(),
            ]);
        }
        return Err(error);
    }
    Ok(())
}
pub fn install(data: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err("automatic service installation currently supports macOS only; run shuttle listen under your process manager on other systems".into());
    }
    let uid = Command::new("/usr/bin/id").arg("-u").output()?;
    let uid = std::str::from_utf8(&uid.stdout)?.trim();
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid user domain".into());
    }
    install_with(
        data,
        &config::home(),
        &fs::read(std::env::current_exe()?)?,
        &std::env::var("PATH").unwrap_or_default(),
        &format!("gui/{uid}"),
        system,
    )?;
    println!("Shuttle 后台服务已安装并启用登录自启动。运行 shuttle tasks 查看任务。");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    #[test]
    fn migrates_legacy_worker_and_command_without_copying_private_state() {
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join("Library/Application Support/AgentRelay");
        config::write(&data.join("config.json"), b"PRIVATE_FIXTURE_CONFIG").unwrap();
        config::write(&data.join("tasks.sqlite3"), b"PRIVATE_FIXTURE_QUEUE").unwrap();
        let command = home.path().join(".local/bin/agent-relay");
        link(&data.join("bin/agent-relay"), &command).unwrap();
        let old_unit = home
            .path()
            .join(format!("Library/LaunchAgents/{LEGACY_LABEL}.plist"));
        config::write(&old_unit, b"old login item").unwrap();
        let calls = RefCell::new(Vec::new());
        let old_running = RefCell::new(true);
        install_with(
            &data,
            home.path(),
            b"SYNTHETIC_BINARY",
            "/bin",
            "gui/123",
            |args| {
                calls.borrow_mut().push(args.to_vec());
                if args[0] == "print" {
                    return args[1].ends_with(LEGACY_LABEL) && *old_running.borrow();
                }
                if args[0] == "bootout" {
                    *old_running.borrow_mut() = false;
                }
                true
            },
        )
        .unwrap();
        assert_eq!(fs::read_link(&command).unwrap(), data.join("bin/shuttle"));
        assert_eq!(
            fs::read_link(home.path().join(".local/bin/shuttle")).unwrap(),
            data.join("bin/shuttle")
        );
        assert!(!old_unit.exists());
        assert_eq!(
            fs::read(data.join("config.json")).unwrap(),
            b"PRIVATE_FIXTURE_CONFIG"
        );
        assert_eq!(
            fs::read(data.join("tasks.sqlite3")).unwrap(),
            b"PRIVATE_FIXTURE_QUEUE"
        );
        let calls = calls.borrow();
        let stop = calls
            .iter()
            .position(|a| a[0] == "bootout" && a[1].ends_with(LEGACY_LABEL))
            .unwrap();
        let start = calls.iter().position(|a| a[0] == "bootstrap").unwrap();
        assert!(stop < start);
    }
    #[test]
    fn failed_migration_restores_legacy_login_item_and_command() {
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join("data");
        let command = home.path().join(".local/bin/agent-relay");
        let original = data.join("bin/agent-relay");
        link(&original, &command).unwrap();
        let old_unit = home
            .path()
            .join(format!("Library/LaunchAgents/{LEGACY_LABEL}.plist"));
        config::write(&old_unit, b"old login item").unwrap();
        let old_running = RefCell::new(true);
        let calls = RefCell::new(Vec::new());
        assert!(
            install_with(&data, home.path(), b"BINARY", "/bin", "gui/123", |args| {
                calls.borrow_mut().push(args.to_vec());
                if args[0] == "print" {
                    return args[1].ends_with(LEGACY_LABEL) && *old_running.borrow();
                }
                if args[0] == "bootout" {
                    *old_running.borrow_mut() = false;
                }
                args[0] != "bootstrap" || args[2].ends_with(&format!("{LEGACY_LABEL}.plist"))
            })
            .is_err()
        );
        assert_eq!(fs::read_link(command).unwrap(), original);
        assert!(!home.path().join(".local/bin/shuttle").exists());
        assert_eq!(fs::read(old_unit).unwrap(), b"old login item");
        assert!(
            calls
                .borrow()
                .iter()
                .any(|a| a[0] == "bootstrap" && a[2].ends_with(&format!("{LEGACY_LABEL}.plist")))
        );
    }
    #[test]
    fn unrelated_command_is_not_overwritten_and_paths_are_escaped() {
        let home = tempfile::tempdir().unwrap();
        let cmd = home.path().join(".local/bin/shuttle");
        config::write(&cmd, b"UNRELATED").unwrap();
        assert!(
            install_with(
                &home.path().join("data"),
                home.path(),
                b"BINARY",
                "/bin",
                "gui/123",
                |_| panic!("no service changes before preflight")
            )
            .is_err()
        );
        assert_eq!(fs::read(cmd).unwrap(), b"UNRELATED");
        assert!(
            unit_text(Path::new("/tmp/a&b/shuttle"), Path::new("/tmp/a'b"), "/bin")
                .contains("a&amp;b")
        );
    }
}
