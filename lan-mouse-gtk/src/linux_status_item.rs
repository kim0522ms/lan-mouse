use std::{cell::RefCell, env};

use adw::prelude::*;
use gtk::{gio, glib};
use ksni::blocking::{Handle, TrayMethods};

use crate::window::Window;

enum TrayCommand {
    Present,
    QuitCompletely,
}

struct StatusItem {
    _hold: gio::ApplicationHoldGuard,
    _handle: Handle<LanMouseTray>,
}

struct LanMouseTray {
    sender: async_channel::Sender<TrayCommand>,
}

thread_local! {
    static STATUS_ITEM: RefCell<Option<StatusItem>> = const { RefCell::new(None) };
}

pub fn setup(app: &adw::Application, window: &Window) -> bool {
    let (sender, receiver) = async_channel::unbounded();
    let tray = LanMouseTray { sender };
    let handle = match tray.spawn() {
        Ok(handle) => handle,
        Err(err) => {
            log::warn!("Linux status item unavailable: {err}");
            return false;
        }
    };

    let hold = app.hold();
    STATUS_ITEM.with(|item| {
        item.replace(Some(StatusItem {
            _hold: hold,
            _handle: handle,
        }));
    });

    glib::spawn_future_local({
        let app = app.downgrade();
        let window = window.downgrade();
        async move {
            while let Ok(command) = receiver.recv().await {
                let Some(app) = app.upgrade() else {
                    break;
                };
                let Some(window) = window.upgrade() else {
                    break;
                };

                match command {
                    TrayCommand::Present => window.present(),
                    TrayCommand::QuitCompletely => {
                        window.request_shutdown();
                        app.quit();
                    }
                }
            }
        }
    });

    true
}

impl ksni::Tray for LanMouseTray {
    fn id(&self) -> String {
        "de.feschber.LanMouse".into()
    }

    fn title(&self) -> String {
        "Lan Mouse Ex".into()
    }

    fn icon_name(&self) -> String {
        "de.feschber.LanMouse".into()
    }

    fn icon_theme_path(&self) -> String {
        env::var_os("HOME")
            .map(|home| {
                std::path::PathBuf::from(home)
                    .join(".local/share/icons/hicolor")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_default()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Lan Mouse Ex".into(),
            description: "Mouse and keyboard sharing".into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        send_command(&self.sender, TrayCommand::Present);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{Disposition, StandardItem};

        vec![
            StandardItem {
                label: "Open Lan Mouse".into(),
                icon_name: "window-new-symbolic".into(),
                activate: Box::new(|tray: &mut Self| {
                    send_command(&tray.sender, TrayCommand::Present)
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit Completely".into(),
                icon_name: "application-exit-symbolic".into(),
                disposition: Disposition::Warning,
                activate: Box::new(|tray: &mut Self| {
                    send_command(&tray.sender, TrayCommand::QuitCompletely)
                }),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        log::warn!("Linux status item watcher offline: {reason:?}");
        true
    }
}

fn send_command(sender: &async_channel::Sender<TrayCommand>, command: TrayCommand) {
    if let Err(err) = sender.try_send(command) {
        log::warn!("failed to dispatch Linux status item command: {err}");
    }
}
