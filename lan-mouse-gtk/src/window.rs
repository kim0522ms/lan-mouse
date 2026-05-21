mod imp;

use std::{
    collections::HashMap,
    env, fs, io,
    path::{Path, PathBuf},
};

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::{Object, clone};
use gtk::{
    Align, Box as GtkBox, Label, NoSelection, Orientation, Separator, Window as GtkWindow, gio,
    glib::{self, closure_local},
};

use lan_mouse_ipc::{
    ClientConfig, ClientHandle, ClientState, DEFAULT_PORT, FrontendRequest, FrontendRequestWriter,
    Position,
};

use crate::{
    authorization_window::AuthorizationWindow, fingerprint_window::FingerprintWindow,
    key_object::KeyObject, key_row::KeyRow,
};

use super::{client_object::ClientObject, client_row::ClientRow};

#[cfg(target_os = "macos")]
fn set_button_content_label(button: &gtk::Button, label: &str) {
    // The Reenable/Grant/Relaunch button wraps its icon+label in an
    // AdwButtonContent (see window.ui). Walk into it and swap the label
    // rather than GtkButton::set_label, which would replace the content
    // widget and drop the icon.
    if let Some(content) = button.child().and_downcast::<adw::ButtonContent>() {
        content.set_label(label);
    }
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends adw::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl Window {
    pub(super) fn new(app: &adw::Application, conn: FrontendRequestWriter) -> Self {
        let window: Self = Object::builder().property("application", app).build();
        window
            .imp()
            .frontend_request_writer
            .borrow_mut()
            .replace(conn);
        window
    }

    fn clients(&self) -> gio::ListStore {
        self.imp()
            .clients
            .borrow()
            .clone()
            .expect("Could not get clients")
    }

    fn authorized(&self) -> gio::ListStore {
        self.imp()
            .authorized
            .borrow()
            .clone()
            .expect("Could not get authorized")
    }

    fn client_by_idx(&self, idx: u32) -> Option<ClientObject> {
        self.clients().item(idx).map(|o| o.downcast().unwrap())
    }

    fn authorized_by_idx(&self, idx: u32) -> Option<KeyObject> {
        self.authorized().item(idx).map(|o| o.downcast().unwrap())
    }

    fn row_by_idx(&self, idx: i32) -> Option<ClientRow> {
        self.imp()
            .client_list
            .get()
            .row_at_index(idx)
            .map(|o| o.downcast().expect("expected ClientRow"))
    }

    fn setup_authorized(&self) {
        let store = gio::ListStore::new::<KeyObject>();
        self.imp().authorized.replace(Some(store));
        let selection_model = NoSelection::new(Some(self.authorized()));
        self.imp().authorized_list.bind_model(
            Some(&selection_model),
            clone!(
                #[weak(rename_to = window)]
                self,
                #[upgrade_or_panic]
                move |obj| {
                    let key_obj = obj.downcast_ref().expect("object of type `KeyObject`");
                    let row = window.create_key_row(key_obj);
                    row.connect_closure(
                        "request-delete",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: KeyRow| {
                                if let Some(key_obj) = window.authorized_by_idx(row.index() as u32)
                                {
                                    window.request_fingerprint_remove(key_obj.get_fingerprint());
                                }
                            }
                        ),
                    );
                    row.upcast()
                }
            ),
        )
    }

    fn setup_clients(&self) {
        let model = gio::ListStore::new::<ClientObject>();
        self.imp().clients.replace(Some(model));

        let selection_model = NoSelection::new(Some(self.clients()));
        self.imp().client_list.bind_model(
            Some(&selection_model),
            clone!(
                #[weak(rename_to = window)]
                self,
                #[upgrade_or_panic]
                move |obj| {
                    let client_object = obj
                        .downcast_ref()
                        .expect("Expected object of type `ClientObject`.");
                    let row = window.create_client_row(client_object);
                    row.connect_closure(
                        "request-hostname-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow, hostname: String| {
                                log::debug!("request-hostname-change");
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    let hostname = Some(hostname).filter(|s| !s.is_empty());
                                    /* changed in response to FrontendEvent
                                     * -> do not request additional update */
                                    window.request(FrontendRequest::UpdateHostname(
                                        client.handle(),
                                        hostname,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-port-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow, port: u32| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    window.request(FrontendRequest::UpdatePort(
                                        client.handle(),
                                        port as u16,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-activate",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow, active: bool| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    log::debug!(
                                        "request: {} client",
                                        if active { "activating" } else { "deactivating" }
                                    );
                                    window.request(FrontendRequest::Activate(
                                        client.handle(),
                                        active,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-delete",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    window.request(FrontendRequest::Delete(client.handle()));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-dns",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    window.request(FrontendRequest::ResolveDns(
                                        client.get_data().handle,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-position-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow, pos_idx: u32| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    let position = match pos_idx {
                                        0 => Position::Left,
                                        1 => Position::Right,
                                        2 => Position::Top,
                                        _ => Position::Bottom,
                                    };
                                    window.request(FrontendRequest::UpdatePosition(
                                        client.handle(),
                                        position,
                                    ));
                                }
                            }
                        ),
                    );
                    row.upcast()
                }
            ),
        );
    }

    fn setup_icon(&self) {
        self.set_icon_name(Some("de.feschber.LanMouse"));
    }

    /// workaround for a bug in libadwaita that shows an ugly line beneath
    /// the last element if a placeholder is set.
    /// https://gitlab.gnome.org/GNOME/gtk/-/merge_requests/6308
    fn update_placeholder_visibility(&self) {
        let visible = self.clients().n_items() == 0;
        let placeholder = self.imp().client_placeholder.get();
        self.imp().client_list.set_placeholder(match visible {
            true => Some(&placeholder),
            false => None,
        });
    }

    fn update_auth_placeholder_visibility(&self) {
        let visible = self.authorized().n_items() == 0;
        let placeholder = self.imp().authorized_placeholder.get();
        self.imp().authorized_list.set_placeholder(match visible {
            true => Some(&placeholder),
            false => None,
        });
    }

    fn create_client_row(&self, client_object: &ClientObject) -> ClientRow {
        let row = ClientRow::new(client_object);
        row.bind(client_object);
        row
    }

    fn create_key_row(&self, key_object: &KeyObject) -> KeyRow {
        let row = KeyRow::new();
        row.bind(key_object);
        row
    }

    pub(super) fn new_client(
        &self,
        handle: ClientHandle,
        client: ClientConfig,
        state: ClientState,
    ) {
        let client = ClientObject::new(handle, client, state.clone());
        self.clients().append(&client);
        self.update_placeholder_visibility();
        self.update_dns_state(handle, !state.ips.is_empty());
    }

    pub(super) fn update_client_list(
        &self,
        clients: Vec<(ClientHandle, ClientConfig, ClientState)>,
    ) {
        for (handle, client, state) in clients {
            if self.client_idx(handle).is_some() {
                self.update_client_config(handle, client);
                self.update_client_state(handle, state);
            } else {
                self.new_client(handle, client, state);
            }
        }
    }

    pub(super) fn update_port(&self, port: u16, msg: Option<String>) {
        if let Some(msg) = msg {
            self.show_toast(msg.as_str());
        }
        self.imp().set_port(port);
    }

    fn client_idx(&self, handle: ClientHandle) -> Option<usize> {
        self.clients()
            .iter::<ClientObject>()
            .position(|c| c.ok().map(|c| c.handle() == handle).unwrap_or_default())
    }

    pub(super) fn delete_client(&self, handle: ClientHandle) {
        let Some(idx) = self.client_idx(handle) else {
            log::warn!("could not find client with handle {handle}");
            return;
        };

        self.clients().remove(idx as u32);
        if self.clients().n_items() == 0 {
            self.update_placeholder_visibility();
        }
    }

    pub(super) fn update_client_config(&self, handle: ClientHandle, client: ClientConfig) {
        let Some(row) = self.row_for_handle(handle) else {
            log::warn!("could not find row for handle {handle}");
            return;
        };
        row.set_hostname(client.hostname);
        row.set_port(client.port);
        row.set_position(client.pos);
    }

    pub(super) fn update_client_state(&self, handle: ClientHandle, state: ClientState) {
        let Some(row) = self.row_for_handle(handle) else {
            log::warn!("could not find row for handle {handle}");
            return;
        };
        let Some(client_object) = self.client_object_for_handle(handle) else {
            log::warn!("could not find row for handle {handle}");
            return;
        };

        /* activation state */
        row.set_active(state.active);

        /* dns state */
        client_object.set_resolving(state.resolving);

        self.update_dns_state(handle, !state.ips.is_empty());
        let ips = state
            .ips
            .into_iter()
            .map(|ip| ip.to_string())
            .collect::<Vec<_>>();
        client_object.set_ips(ips);
    }

    fn client_object_for_handle(&self, handle: ClientHandle) -> Option<ClientObject> {
        self.client_idx(handle)
            .and_then(|i| self.client_by_idx(i as u32))
    }

    fn row_for_handle(&self, handle: ClientHandle) -> Option<ClientRow> {
        self.client_idx(handle)
            .and_then(|i| self.row_by_idx(i as i32))
    }

    fn update_dns_state(&self, handle: ClientHandle, resolved: bool) {
        if let Some(client_row) = self.row_for_handle(handle) {
            client_row.set_dns_state(resolved);
        }
    }

    fn request_port_change(&self) {
        let port = self
            .imp()
            .port_entry
            .get()
            .text()
            .as_str()
            .parse::<u16>()
            .unwrap_or(DEFAULT_PORT);
        self.request(FrontendRequest::ChangePort(port));
    }

    fn request_capture(&self) {
        self.request(FrontendRequest::EnableCapture);
    }

    fn request_emulation(&self) {
        self.request(FrontendRequest::EnableEmulation);
    }

    pub(super) fn request_clipboard_sharing(&self, enabled: bool) {
        self.request(FrontendRequest::SetClipboardSharing(enabled));
    }

    pub(super) fn request_swap_option_command(&self, enabled: bool) {
        self.request(FrontendRequest::SetSwapOptionCommand(enabled));
    }

    pub(super) fn request_sync_lock(&self, enabled: bool) {
        self.request(FrontendRequest::SetSyncLock(enabled));
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn request_sync_lock_broadcast(&self) {
        self.request(FrontendRequest::BroadcastSyncLock);
    }

    pub(super) fn request_autostart(&self, enabled: bool) {
        let result = set_autostart(enabled);
        match result {
            Ok(()) => self.set_autostart(enabled),
            Err(e) => {
                self.sync_autostart_state();
                self.show_toast(format!("autostart update failed: {e}").as_str());
            }
        }
    }

    pub(super) fn sync_autostart_state(&self) {
        self.set_autostart(autostart_enabled());
    }

    pub(super) fn note_event(&self, event: impl Into<String>) {
        self.imp().last_event.replace(event.into());
    }

    fn request_client_create(&self) {
        self.request(FrontendRequest::Create);
    }

    fn open_fingerprint_dialog(&self, fp: Option<String>) {
        let window = FingerprintWindow::new(fp);
        window.set_transient_for(Some(self));
        window.connect_closure(
            "confirm-clicked",
            false,
            closure_local!(
                #[strong(rename_to = parent)]
                self,
                move |w: FingerprintWindow, desc: String, fp: String| {
                    parent.request_fingerprint_add(desc, fp);
                    w.close();
                }
            ),
        );
        window.present();
    }

    fn request_fingerprint_add(&self, desc: String, fp: String) {
        self.request(FrontendRequest::AuthorizeKey(desc, fp));
    }

    fn request_fingerprint_remove(&self, fp: String) {
        self.request(FrontendRequest::RemoveAuthorizedKey(fp));
    }

    fn request(&self, request: FrontendRequest) {
        let mut requester = self.imp().frontend_request_writer.borrow_mut();
        let requester = requester.as_mut().unwrap();
        if let Err(e) = requester.request(request) {
            log::error!("error sending message: {e}");
        };
    }

    pub(super) fn request_shutdown(&self) {
        self.request(FrontendRequest::Shutdown);
    }

    pub(super) fn show_toast(&self, msg: &str) {
        let toast = adw::Toast::new(msg);
        self.add_toast(toast);
    }

    pub(super) fn add_toast(&self, toast: adw::Toast) {
        let toast_overlay = &self.imp().toast_overlay;
        toast_overlay.add_toast(toast);
    }

    pub(super) fn show_diagnostics(&self) {
        let dialog = GtkWindow::builder()
            .title("Connection Diagnostics")
            .modal(true)
            .transient_for(self)
            .default_width(460)
            .build();

        let content = GtkBox::new(Orientation::Vertical, 12);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(18);
        content.set_margin_end(18);

        let title = Label::new(Some("Connection Diagnostics"));
        title.set_xalign(0.0);
        title.add_css_class("title-2");
        content.append(&title);

        add_diagnostics_row(
            &content,
            "Capture",
            enabled_label(self.imp().capture_active.get()),
        );
        add_diagnostics_row(
            &content,
            "Emulation",
            enabled_label(self.imp().emulation_active.get()),
        );
        add_diagnostics_row(
            &content,
            "Clipboard",
            enabled_label(self.imp().clipboard_sharing_switch.is_active()),
        );
        add_diagnostics_row(
            &content,
            "Autostart",
            enabled_label(self.imp().autostart_switch.is_active()),
        );
        add_diagnostics_row(
            &content,
            "Option/Command swap",
            enabled_label(self.imp().swap_option_command_switch.is_active()),
        );
        add_diagnostics_row(
            &content,
            "Sync Lock",
            enabled_label(self.imp().sync_lock_switch.is_active()),
        );
        add_diagnostics_row(&content, "Port", &self.imp().port.get().to_string());

        content.append(&Separator::new(Orientation::Horizontal));

        let clients = self.clients();
        add_diagnostics_row(&content, "Configured peers", &clients.n_items().to_string());
        for client in clients
            .iter::<ClientObject>()
            .filter_map(Result::ok)
            .take(5)
        {
            let data = client.get_data();
            let name = data
                .hostname
                .filter(|hostname| !hostname.is_empty())
                .unwrap_or_else(|| format!("client {}", data.handle));
            let ips = if data.ips.is_empty() {
                "unresolved".to_owned()
            } else {
                data.ips.join(", ")
            };
            let state = if data.active { "active" } else { "inactive" };
            add_diagnostics_row(
                &content,
                &name,
                &format!("{state}, {}, {ips}", data.position),
            );
        }

        add_diagnostics_row(
            &content,
            "Authorized peers",
            &self.authorized().n_items().to_string(),
        );
        let last_event = self.imp().last_event.borrow();
        add_diagnostics_row(
            &content,
            "Last event",
            if last_event.is_empty() {
                "none"
            } else {
                last_event.as_str()
            },
        );

        let close_button = gtk::Button::with_label("Close");
        close_button.set_halign(Align::End);
        close_button.connect_clicked(clone!(
            #[weak]
            dialog,
            move |_| dialog.close()
        ));
        content.append(&close_button);

        dialog.set_child(Some(&content));
        dialog.present();
    }

    pub(super) fn set_capture(&self, active: bool) {
        self.imp().capture_active.replace(active);
        self.update_capture_emulation_status();
    }

    pub(super) fn set_emulation(&self, active: bool) {
        self.imp().emulation_active.replace(active);
        self.update_capture_emulation_status();
    }

    pub(super) fn set_clipboard_sharing(&self, enabled: bool) {
        let switch = self.imp().clipboard_sharing_switch.get();
        self.imp().clipboard_sharing_syncing.set(true);
        if switch.is_active() != enabled {
            switch.set_active(enabled);
        }
        if switch.state() != enabled {
            switch.set_state(enabled);
        }
        self.imp().clipboard_sharing_syncing.set(false);
    }

    fn set_autostart(&self, enabled: bool) {
        let switch = self.imp().autostart_switch.get();
        self.imp().autostart_syncing.set(true);
        if switch.is_active() != enabled {
            switch.set_active(enabled);
        }
        if switch.state() != enabled {
            switch.set_state(enabled);
        }
        self.imp().autostart_syncing.set(false);
    }

    pub(super) fn set_swap_option_command(&self, enabled: bool) {
        let switch = self.imp().swap_option_command_switch.get();
        self.imp().swap_option_command_syncing.set(true);
        if switch.is_active() != enabled {
            switch.set_active(enabled);
        }
        if switch.state() != enabled {
            switch.set_state(enabled);
        }
        self.imp().swap_option_command_syncing.set(false);
    }

    pub(super) fn set_sync_lock(&self, enabled: bool) {
        let switch = self.imp().sync_lock_switch.get();
        self.imp().sync_lock_syncing.set(true);
        if switch.is_active() != enabled {
            switch.set_active(enabled);
        }
        if switch.state() != enabled {
            switch.set_state(enabled);
        }
        self.imp().sync_lock_syncing.set(false);
    }

    #[cfg(target_os = "macos")]
    pub(super) fn refresh_capture_emulation_status(&self) {
        self.update_capture_emulation_status();
    }

    fn update_capture_emulation_status(&self) {
        let capture = self.imp().capture_active.get();
        let emulation = self.imp().emulation_active.get();

        #[cfg(target_os = "macos")]
        {
            // On macOS, capture and emulation share the same TCC gate
            // (Accessibility). Collapse to a single warning row —
            // emulation_status_row stays hidden and capture_status_row
            // doubles as the shared status indicator. Its text and
            // button mutate based on whether we're waiting for AX or
            // waiting for the user to relaunch the app.
            let anything_off = !capture || !emulation;
            self.imp().emulation_status_row.set_visible(false);
            self.imp().capture_status_row.set_visible(anything_off);
            self.imp().capture_emulation_group.set_visible(anything_off);

            if anything_off {
                self.update_macos_warning_row_text();
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            self.imp().capture_status_row.set_visible(!capture);
            self.imp().emulation_status_row.set_visible(!emulation);
            self.imp()
                .capture_emulation_group
                .set_visible(!capture || !emulation);
        }
    }

    #[cfg(target_os = "macos")]
    fn update_macos_warning_row_text(&self) {
        let row = &self.imp().capture_status_row;
        let button = &self.imp().input_capture_button;

        if crate::macos_privacy::accessibility_granted() {
            // AX granted but capture/emulation still off → the daemon
            // subprocess bailed at startup and needs a fresh process to
            // re-initialize with the new grant in place.
            row.set_title("relaunch required");
            row.set_subtitle("Accessibility granted — restart to activate capture and emulation");
            set_button_content_label(button, "Relaunch");
        } else {
            // AX missing → send the user to System Settings.
            row.set_title("input capture is disabled");
            row.set_subtitle("grant Accessibility permission to enable");
            set_button_content_label(button, "Grant");
        }
    }

    pub(super) fn set_authorized_keys(&self, fingerprints: HashMap<String, String>) {
        let authorized = self.authorized();
        // clear list
        authorized.remove_all();
        // insert fingerprints
        for (fingerprint, description) in fingerprints {
            let key_obj = KeyObject::new(description, fingerprint);
            authorized.append(&key_obj);
        }
        self.update_auth_placeholder_visibility();
    }

    pub(super) fn set_pk_fp(&self, fingerprint: &str) {
        self.imp().fingerprint_row.set_subtitle(fingerprint);
    }

    pub(super) fn request_authorization(&self, fingerprint: &str) {
        if let Some(w) = self.imp().authorization_window.borrow_mut().take() {
            w.close();
        }
        let window = AuthorizationWindow::new(fingerprint);
        window.set_transient_for(Some(self));
        window.connect_closure(
            "confirm-clicked",
            false,
            closure_local!(
                #[strong(rename_to = parent)]
                self,
                move |w: AuthorizationWindow, fp: String| {
                    w.close();
                    parent.open_fingerprint_dialog(Some(fp));
                }
            ),
        );
        window.connect_closure(
            "cancel-clicked",
            false,
            closure_local!(move |w: AuthorizationWindow| {
                w.close();
            }),
        );
        window.present();
        self.imp().authorization_window.replace(Some(window));
    }
}

fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn add_diagnostics_row(content: &GtkBox, name: &str, value: &str) {
    let row = GtkBox::new(Orientation::Horizontal, 12);

    let name_label = Label::new(Some(name));
    name_label.set_xalign(0.0);
    name_label.set_hexpand(true);
    name_label.add_css_class("dim-label");

    let value_label = Label::new(Some(value));
    value_label.set_xalign(1.0);
    value_label.set_selectable(true);
    value_label.set_wrap(true);
    value_label.set_max_width_chars(32);

    row.append(&name_label);
    row.append(&value_label);
    content.append(&row);
}

fn autostart_enabled() -> bool {
    let Some(path) = autostart_file() else {
        return false;
    };
    if !path.exists() {
        return false;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        fs::read_to_string(path)
            .map(|content| !desktop_entry_hidden(&content))
            .unwrap_or(false)
    }

    #[cfg(any(target_os = "macos", windows))]
    {
        true
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_entry_hidden(content: &str) -> bool {
    content.lines().any(|line| {
        let line = line.trim();
        line.eq_ignore_ascii_case("Hidden=true")
    })
}

fn set_autostart(enabled: bool) -> io::Result<()> {
    let Some(path) = autostart_file() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "could not determine config directory",
        ));
    };

    if enabled {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, autostart_entry()?)
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn autostart_file() -> Option<PathBuf> {
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".config")))?;
    Some(
        config_home
            .join("autostart")
            .join("de.feschber.LanMouse.desktop"),
    )
}

#[cfg(target_os = "macos")]
fn autostart_file() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| {
        Path::new(&home)
            .join("Library")
            .join("LaunchAgents")
            .join("de.feschber.LanMouse.plist")
    })
}

#[cfg(windows)]
fn autostart_file() -> Option<PathBuf> {
    env::var_os("APPDATA").map(|appdata| {
        Path::new(&appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
            .join("Lan Mouse Ex.cmd")
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn autostart_entry() -> io::Result<String> {
    let exe = env::current_exe()?;
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Lan Mouse Ex\n\
         Comment=Mouse & Keyboard sharing via LAN\n\
         Exec={}\n\
         TryExec={}\n\
         Icon=de.feschber.LanMouse\n\
         Terminal=false\n\
         StartupNotify=true\n\
         X-GNOME-Autostart-enabled=true\n",
        desktop_exec_path(&exe),
        exe.display()
    ))
}

#[cfg(target_os = "macos")]
fn autostart_entry() -> io::Result<String> {
    let exe = env::current_exe()?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         <key>Label</key>\n\
         <string>de.feschber.LanMouse</string>\n\
         <key>ProgramArguments</key>\n\
         <array>\n\
         <string>{}</string>\n\
         </array>\n\
         <key>RunAtLoad</key>\n\
         <true/>\n\
         </dict>\n\
         </plist>\n",
        xml_escape(&exe.to_string_lossy())
    ))
}

#[cfg(windows)]
fn autostart_entry() -> io::Result<String> {
    let exe = env::current_exe()?;
    Ok(format!(
        "@echo off\r\n\
         start \"Lan Mouse Ex\" \"{}\"\r\n",
        exe.to_string_lossy().replace('"', "\"\"")
    ))
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_exec_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if path.contains(char::is_whitespace) {
        format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        path.into_owned()
    }
}
