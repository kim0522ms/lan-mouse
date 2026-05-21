#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::{process::Command, sync::mpsc, task::JoinHandle};

pub(crate) struct LockMonitor {
    rx: Option<mpsc::UnboundedReceiver<()>>,
    _tasks: Vec<JoinHandle<()>>,
}

impl LockMonitor {
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let tasks = platform_monitor_tasks(tx);
        let rx = if tasks.is_empty() { None } else { Some(rx) };
        Self { rx, _tasks: tasks }
    }

    pub(crate) async fn event(&mut self) {
        if let Some(rx) = &mut self.rx {
            if rx.recv().await.is_none() {
                std::future::pending::<()>().await;
            }
        } else {
            std::future::pending::<()>().await;
        }
    }
}

pub(crate) fn lock_session() {
    tokio::spawn(async {
        if let Err(err) = platform_lock_session().await {
            log::warn!("failed to lock local session: {err}");
        }
    });
}

#[cfg(target_os = "linux")]
fn platform_monitor_tasks(tx: mpsc::UnboundedSender<()>) -> Vec<JoinHandle<()>> {
    vec![tokio::spawn(monitor_dbus_screensaver(tx))]
}

#[cfg(not(target_os = "linux"))]
fn platform_monitor_tasks(_tx: mpsc::UnboundedSender<()>) -> Vec<JoinHandle<()>> {
    Vec::new()
}

#[cfg(target_os = "linux")]
async fn monitor_dbus_screensaver(tx: mpsc::UnboundedSender<()>) {
    let mut child = match Command::new("dbus-monitor")
        .arg("--session")
        .arg("type='signal',interface='org.freedesktop.ScreenSaver'")
        .arg("type='signal',interface='org.gnome.ScreenSaver'")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            log::warn!("sync lock monitor unavailable: {err}");
            return;
        }
    };

    let Some(stdout) = child.stdout.take() else {
        log::warn!("sync lock monitor unavailable: stdout was not captured");
        return;
    };

    let mut lines = BufReader::new(stdout).lines();
    let mut active_changed = false;
    let mut last = Instant::now() - Duration::from_secs(10);

    while let Ok(Some(line)) = lines.next_line().await {
        if line.contains("member=ActiveChanged") || line.contains("member=SessionLocked") {
            active_changed = true;
            if line.contains("SessionLocked") && last.elapsed() > Duration::from_secs(2) {
                last = Instant::now();
                let _ = tx.send(());
            }
            continue;
        }

        if active_changed && line.contains("boolean true") {
            active_changed = false;
            if last.elapsed() > Duration::from_secs(2) {
                last = Instant::now();
                let _ = tx.send(());
            }
        } else if active_changed && line.contains("boolean false") {
            active_changed = false;
        }
    }

    let _ = child.kill().await;
    log::warn!("sync lock monitor exited");
}

#[cfg(target_os = "linux")]
async fn platform_lock_session() -> std::io::Result<()> {
    let status = Command::new("loginctl")
        .arg("lock-session")
        .status()
        .await?;
    if !status.success() {
        log::warn!("loginctl lock-session exited with {status}");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn platform_lock_session() -> std::io::Result<()> {
    let status = Command::new(
        "/System/Library/CoreServices/Menu Extras/User.menu/Contents/Resources/CGSession",
    )
    .arg("-suspend")
    .status()
    .await?;
    if !status.success() {
        log::warn!("CGSession -suspend exited with {status}");
    }
    Ok(())
}

#[cfg(windows)]
async fn platform_lock_session() -> std::io::Result<()> {
    let status = Command::new("rundll32.exe")
        .arg("user32.dll,LockWorkStation")
        .status()
        .await?;
    if !status.success() {
        log::warn!("LockWorkStation exited with {status}");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
async fn platform_lock_session() -> std::io::Result<()> {
    log::warn!("sync lock is not supported on this platform");
    Ok(())
}
