use std::{
    ffi::{CStr, CString, c_char, c_void},
    ptr,
    sync::{Mutex, OnceLock},
};

use gtk::{glib, prelude::*};

use crate::window::Window;

type Id = *mut c_void;
type Class = *mut c_void;
type Sel = *mut c_void;
type Bool = i8;

static LOCK_SENDER: OnceLock<Mutex<Option<async_channel::Sender<()>>>> = OnceLock::new();
static LOCK_OBSERVER: OnceLock<usize> = OnceLock::new();
static REGISTERED: OnceLock<()> = OnceLock::new();
const NS_NOTIFICATION_SUSPENSION_BEHAVIOR_DELIVER_IMMEDIATELY: u64 = 4;

pub(crate) fn setup(window: &Window) {
    let (sender, receiver) = async_channel::bounded(1);
    LOCK_SENDER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("lock sender mutex")
        .replace(sender);

    REGISTERED.get_or_init(register_observer);

    glib::spawn_future_local({
        let window = window.downgrade();
        async move {
            while receiver.recv().await.is_ok() {
                let Some(window) = window.upgrade() else {
                    break;
                };
                window.request_sync_lock_broadcast();
            }
        }
    });

    log::info!("macOS sync lock monitor registered");
}

fn register_observer() {
    unsafe {
        let center = msg_send_id(
            class(c"NSDistributedNotificationCenter"),
            sel(c"defaultCenter"),
        );
        if center.is_null() {
            log::warn!("sync lock monitor unavailable: NSDistributedNotificationCenter is null");
            return;
        }

        msg_send_void_id_sel_id_id_u64(
            center,
            sel(c"addObserver:selector:name:object:suspensionBehavior:"),
            retained_lock_observer(),
            sel(c"screenIsLocked:"),
            nsstring(c"com.apple.screenIsLocked"),
            ptr::null_mut(),
            NS_NOTIFICATION_SUSPENSION_BEHAVIOR_DELIVER_IMMEDIATELY,
        );
    }
}

unsafe fn retained_lock_observer() -> Id {
    *LOCK_OBSERVER.get_or_init(|| {
        let class = lock_observer_class();
        let observer = msg_send_id(msg_send_id(class, sel(c"alloc")), sel(c"init"));
        assert!(!observer.is_null(), "failed to create sync lock observer");
        observer as usize
    }) as Id
}

fn lock_observer_class() -> Class {
    static CLASS: OnceLock<usize> = OnceLock::new();

    *CLASS.get_or_init(|| unsafe {
        let superclass = class(c"NSObject");
        let class_name = CString::new("LanMouseGtkSyncLockObserver").unwrap();
        let class = objc_allocateClassPair(superclass, class_name.as_ptr(), 0);
        assert!(!class.is_null(), "failed to allocate sync lock observer");
        class_addMethod(
            class,
            sel(c"screenIsLocked:"),
            screen_is_locked as *const c_void,
            c"v@:@".as_ptr(),
        );
        objc_registerClassPair(class);
        class as usize
    }) as Class
}

extern "C" fn screen_is_locked(_this: Id, _cmd: Sel, _notification: Id) {
    log::info!("macOS screen lock notification received");
    if let Some(sender) = LOCK_SENDER
        .get()
        .and_then(|sender| sender.lock().ok())
        .and_then(|sender| sender.clone())
    {
        let _ = sender.try_send(());
    }
}

unsafe fn class(name: &CStr) -> Class {
    let class = objc_getClass(name.as_ptr());
    assert!(!class.is_null(), "missing Objective-C class {name:?}");
    class
}

unsafe fn sel(name: &CStr) -> Sel {
    sel_registerName(name.as_ptr())
}

unsafe fn nsstring(value: &CStr) -> Id {
    msg_send_id_ptr(
        class(c"NSString"),
        sel(c"stringWithUTF8String:"),
        value.as_ptr(),
    )
}

#[link(name = "objc")]
extern "C" {
    fn objc_allocateClassPair(superclass: Class, name: *const c_char, extra_bytes: usize) -> Class;
    fn objc_getClass(name: *const c_char) -> Class;
    fn objc_registerClassPair(class: Class);
    fn sel_registerName(name: *const c_char) -> Sel;
    fn class_addMethod(class: Class, name: Sel, imp: *const c_void, types: *const c_char) -> Bool;
}

#[link(name = "Foundation", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    #[link_name = "objc_msgSend"]
    fn msg_send_id(receiver: Id, selector: Sel) -> Id;
    #[link_name = "objc_msgSend"]
    fn msg_send_id_ptr(receiver: Id, selector: Sel, value: *const c_char) -> Id;
    #[link_name = "objc_msgSend"]
    fn msg_send_void_id_sel_id_id_u64(
        receiver: Id,
        selector: Sel,
        observer: Id,
        action: Sel,
        name: Id,
        object: Id,
        suspension_behavior: u64,
    );
}
