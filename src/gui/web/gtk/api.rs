//! WebKitGTK's entry points, resolved at run time (09 §2.3): `libwebkit2gtk-4.1.so.0` opened with
//! `dlopen` at the first page view, every GTK, GDK, GLib, GIO, cairo, libsoup and WebKit function
//! below found by `dlsym` on that one handle (its dependencies are searched with it). Nothing is
//! linked, as for sopen's other runtime libraries (`fnkeys/macos.rs`, `remote/local/trash.rs`):
//! `ldd` does not change, and a machine without the library loses only the page view (§4).
//!
//! The C layouts sopen writes into — GDK's input events, `GInputStreamClass` for the page
//! stream's class — are declared here, and the class's sizes are checked against GLib's own
//! `g_type_query` before any is used ([`stream_layout`]).

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CStr, CString};

/// A pointer to a GObject, a boxed or a C struct of the libraries'.
pub(super) type P = *mut c_void;
pub(super) type GBoolean = c_int;
pub(super) type GType = usize;

/// The library the engine loads.
pub(super) const SONAME: &str = "libwebkit2gtk-4.1.so.0";

/// The entry points, each as its header declares it. Variadic ones are called with a NULL after
/// their last property, as GObject asks.
macro_rules! api {
    ($($(#[$meta:meta])* $name:ident: $ty:ty;)*) => {
        pub(super) struct Api {
            $($(#[$meta])* pub(super) $name: $ty,)*
        }

        impl Api {
            /// Every entry point from `handle`; the first one missing is the error.
            ///
            /// SAFETY: `handle` is a live `dlopen` handle; each field's type is the library's
            /// prototype of the symbol it names.
            unsafe fn resolve(handle: P, soname: &str) -> Result<Api, String> {
                Ok(Api {
                    $($name: {
                        let name = concat!(stringify!($name), "\0");
                        // SAFETY: `handle` came from dlopen; the name is a C string.
                        let found = unsafe { libc::dlsym(handle, name.as_ptr().cast()) };
                        if found.is_null() {
                            return Err(format!("{soname} has no {}", stringify!($name)));
                        }
                        // SAFETY: a function pointer the size of a data pointer; the field's
                        // type is the symbol's prototype.
                        unsafe { std::mem::transmute_copy::<P, $ty>(&found) }
                    },)*
                })
            }
        }
    };
}

api! {
    // GLib, GObject, GIO.
    g_main_context_iteration: unsafe extern "C" fn(P, GBoolean) -> GBoolean;
    g_main_context_wakeup: unsafe extern "C" fn(P);
    g_idle_add_full: unsafe extern "C" fn(c_int, P, P, P) -> c_uint;
    g_timeout_add_full: unsafe extern "C" fn(c_int, c_uint, P, P, P) -> c_uint;
    g_signal_connect_data: unsafe extern "C" fn(P, *const c_char, P, P, P, c_uint) -> c_ulong;
    g_object_ref: unsafe extern "C" fn(P) -> P;
    g_object_unref: unsafe extern "C" fn(P);
    g_object_new: unsafe extern "C" fn(GType, *const c_char, ...) -> P;
    g_type_register_static_simple:
        unsafe extern "C" fn(GType, *const c_char, c_uint, P, c_uint, P, c_uint) -> GType;
    g_type_query: unsafe extern "C" fn(GType, *mut GTypeQuery);
    g_input_stream_get_type: unsafe extern "C" fn() -> GType;
    g_task_new: unsafe extern "C" fn(P, P, P, P) -> P;
    g_task_return_int: unsafe extern "C" fn(P, isize);
    g_task_return_boolean: unsafe extern "C" fn(P, GBoolean);
    g_task_return_error: unsafe extern "C" fn(P, *mut GError);
    g_task_propagate_int: unsafe extern "C" fn(P, *mut *mut GError) -> isize;
    g_task_propagate_boolean: unsafe extern "C" fn(P, *mut *mut GError) -> GBoolean;
    g_cancellable_connect: unsafe extern "C" fn(P, P, P, P) -> c_ulong;
    g_cancellable_disconnect: unsafe extern "C" fn(P, c_ulong);
    g_error_new_literal: unsafe extern "C" fn(u32, c_int, *const c_char) -> *mut GError;
    g_error_free: unsafe extern "C" fn(*mut GError);
    g_io_error_quark: unsafe extern "C" fn() -> u32;
    g_quark_to_string: unsafe extern "C" fn(u32) -> *const c_char;
    g_bytes_new: unsafe extern "C" fn(*const c_void, usize) -> P;
    g_bytes_unref: unsafe extern "C" fn(P);
    // GTK, GDK.
    gtk_init_check: unsafe extern "C" fn(*mut c_int, P) -> GBoolean;
    gtk_offscreen_window_new: unsafe extern "C" fn() -> P;
    gtk_offscreen_window_get_surface: unsafe extern "C" fn(P) -> P;
    gtk_container_add: unsafe extern "C" fn(P, P);
    gtk_widget_set_size_request: unsafe extern "C" fn(P, c_int, c_int);
    gtk_window_resize: unsafe extern "C" fn(P, c_int, c_int);
    gtk_widget_show_all: unsafe extern "C" fn(P);
    gtk_widget_get_window: unsafe extern "C" fn(P) -> P;
    gtk_widget_event: unsafe extern "C" fn(P, P) -> GBoolean;
    gtk_widget_destroy: unsafe extern "C" fn(P);
    gtk_widget_grab_focus: unsafe extern "C" fn(P);
    gtk_widget_get_scale_factor: unsafe extern "C" fn(P) -> c_int;
    gdk_event_new: unsafe extern "C" fn(c_int) -> P;
    gdk_event_free: unsafe extern "C" fn(P);
    gdk_event_set_device: unsafe extern "C" fn(P, P);
    gdk_display_get_default: unsafe extern "C" fn() -> P;
    gdk_display_get_default_seat: unsafe extern "C" fn(P) -> P;
    gdk_seat_get_pointer: unsafe extern "C" fn(P) -> P;
    gdk_seat_get_keyboard: unsafe extern "C" fn(P) -> P;
    // cairo.
    cairo_surface_map_to_image: unsafe extern "C" fn(P, P) -> P;
    cairo_surface_unmap_image: unsafe extern "C" fn(P, P);
    cairo_surface_flush: unsafe extern "C" fn(P);
    cairo_image_surface_get_width: unsafe extern "C" fn(P) -> c_int;
    cairo_image_surface_get_height: unsafe extern "C" fn(P) -> c_int;
    cairo_image_surface_get_stride: unsafe extern "C" fn(P) -> c_int;
    cairo_image_surface_get_data: unsafe extern "C" fn(P) -> *const u8;
    // libsoup (the request's and the response's headers).
    soup_message_headers_new: unsafe extern "C" fn(c_int) -> P;
    soup_message_headers_append: unsafe extern "C" fn(P, *const c_char, *const c_char);
    soup_message_headers_get_one: unsafe extern "C" fn(P, *const c_char) -> *const c_char;
    // WebKit.
    webkit_get_major_version: unsafe extern "C" fn() -> c_uint;
    webkit_get_minor_version: unsafe extern "C" fn() -> c_uint;
    webkit_get_micro_version: unsafe extern "C" fn() -> c_uint;
    webkit_web_view_get_type: unsafe extern "C" fn() -> GType;
    webkit_web_context_new_ephemeral: unsafe extern "C" fn() -> P;
    webkit_web_context_register_uri_scheme: unsafe extern "C" fn(P, *const c_char, P, P, P);
    webkit_web_context_set_cache_model: unsafe extern "C" fn(P, c_int);
    webkit_web_context_set_sandbox_enabled: unsafe extern "C" fn(P, GBoolean);
    /// The tests' look at a context.
    #[cfg_attr(not(test), allow(dead_code))]
    webkit_web_context_get_sandbox_enabled: unsafe extern "C" fn(P) -> GBoolean;
    webkit_web_context_get_website_data_manager: unsafe extern "C" fn(P) -> P;
    webkit_website_data_manager_set_network_proxy_settings: unsafe extern "C" fn(P, c_int, P);
    webkit_network_proxy_settings_new: unsafe extern "C" fn(*const c_char, *const *const c_char) -> P;
    webkit_network_proxy_settings_free: unsafe extern "C" fn(P);
    webkit_user_content_manager_new: unsafe extern "C" fn() -> P;
    webkit_user_content_manager_add_filter: unsafe extern "C" fn(P, P);
    webkit_user_content_filter_store_new: unsafe extern "C" fn(*const c_char) -> P;
    webkit_user_content_filter_store_save:
        unsafe extern "C" fn(P, *const c_char, P, P, P, P);
    webkit_user_content_filter_store_save_finish:
        unsafe extern "C" fn(P, P, *mut *mut GError) -> P;
    webkit_settings_new: unsafe extern "C" fn() -> P;
    webkit_settings_set_enable_javascript: unsafe extern "C" fn(P, GBoolean);
    webkit_settings_set_enable_javascript_markup: unsafe extern "C" fn(P, GBoolean);
    webkit_settings_set_javascript_can_open_windows_automatically:
        unsafe extern "C" fn(P, GBoolean);
    webkit_settings_set_enable_developer_extras: unsafe extern "C" fn(P, GBoolean);
    webkit_settings_set_hardware_acceleration_policy: unsafe extern "C" fn(P, c_int);
    webkit_settings_set_allow_file_access_from_file_urls: unsafe extern "C" fn(P, GBoolean);
    webkit_settings_set_allow_universal_access_from_file_urls:
        unsafe extern "C" fn(P, GBoolean);
    webkit_settings_set_enable_webrtc: unsafe extern "C" fn(P, GBoolean);
    webkit_settings_set_enable_dns_prefetching: unsafe extern "C" fn(P, GBoolean);
    webkit_settings_set_enable_page_cache: unsafe extern "C" fn(P, GBoolean);
    webkit_web_view_get_settings: unsafe extern "C" fn(P) -> P;
    /// The tests' look at a view's context.
    #[cfg_attr(not(test), allow(dead_code))]
    webkit_web_view_get_context: unsafe extern "C" fn(P) -> P;
    webkit_web_view_load_uri: unsafe extern "C" fn(P, *const c_char);
    webkit_web_view_reload: unsafe extern "C" fn(P);
    webkit_web_view_stop_loading: unsafe extern "C" fn(P);
    webkit_web_view_get_uri: unsafe extern "C" fn(P) -> *const c_char;
    webkit_web_view_get_main_resource: unsafe extern "C" fn(P) -> P;
    /// The tests' look at a page.
    #[cfg_attr(not(test), allow(dead_code))]
    webkit_web_view_get_title: unsafe extern "C" fn(P) -> *const c_char;
    webkit_web_view_set_zoom_level: unsafe extern "C" fn(P, f64);
    webkit_web_view_set_input_method_context: unsafe extern "C" fn(P, P);
    webkit_policy_decision_use: unsafe extern "C" fn(P);
    webkit_policy_decision_ignore: unsafe extern "C" fn(P);
    webkit_navigation_policy_decision_get_navigation_action: unsafe extern "C" fn(P) -> P;
    webkit_navigation_action_get_request: unsafe extern "C" fn(P) -> P;
    webkit_navigation_action_get_navigation_type: unsafe extern "C" fn(P) -> c_int;
    webkit_response_policy_decision_get_response: unsafe extern "C" fn(P) -> P;
    webkit_response_policy_decision_is_mime_type_supported: unsafe extern "C" fn(P) -> GBoolean;
    webkit_response_policy_decision_is_main_frame_main_resource:
        unsafe extern "C" fn(P) -> GBoolean;
    webkit_uri_response_get_status_code: unsafe extern "C" fn(P) -> c_uint;
    webkit_uri_response_get_mime_type: unsafe extern "C" fn(P) -> *const c_char;
    webkit_uri_request_get_uri: unsafe extern "C" fn(P) -> *const c_char;
    webkit_uri_request_get_http_headers: unsafe extern "C" fn(P) -> P;
    webkit_uri_scheme_request_get_uri: unsafe extern "C" fn(P) -> *const c_char;
    webkit_uri_scheme_request_get_http_headers: unsafe extern "C" fn(P) -> P;
    webkit_uri_scheme_request_finish_with_response: unsafe extern "C" fn(P, P);
    webkit_uri_scheme_request_finish_error: unsafe extern "C" fn(P, *mut GError);
    webkit_uri_scheme_response_new: unsafe extern "C" fn(P, i64) -> P;
    webkit_uri_scheme_response_set_status: unsafe extern "C" fn(P, c_uint, *const c_char);
    webkit_uri_scheme_response_set_content_type: unsafe extern "C" fn(P, *const c_char);
    webkit_uri_scheme_response_set_http_headers: unsafe extern "C" fn(P, P);
    webkit_download_cancel: unsafe extern "C" fn(P);
    webkit_option_menu_close: unsafe extern "C" fn(P);
    webkit_file_chooser_request_cancel: unsafe extern "C" fn(P);
    webkit_color_chooser_request_cancel: unsafe extern "C" fn(P);
    webkit_authentication_request_cancel: unsafe extern "C" fn(P);
}

// SAFETY: function pointers into libraries that stay loaded for the process; calling them is
// each caller's business (GTK's only on the GTK thread, the wakeups from anywhere).
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

/// Opens `soname` and resolves every entry point; why not, at once — a missing library fails in
/// `dlopen` (WV-7: well under a millisecond), a too-old one at its first missing symbol. The
/// handle is never closed (GTK cannot be unloaded).
pub(super) fn load(soname: &str) -> Result<Api, String> {
    let name = CString::new(soname).map_err(|_| format!("not a library name: {soname:?}"))?;
    // SAFETY: a C string naming a library; the handle stays open for the process.
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: dlerror's text (this thread's), read at once.
        let why = unsafe { libc::dlerror() };
        let why = if why.is_null() {
            soname.to_string()
        } else {
            // SAFETY: a C string dlerror returned.
            unsafe { CStr::from_ptr(why) }
                .to_string_lossy()
                .into_owned()
        };
        return Err(format!("cannot load {why}"));
    }
    // SAFETY: the handle just opened; the table's types are the headers' prototypes.
    unsafe { Api::resolve(handle, soname) }
}

/// A C string the libraries returned (NULL is empty), copied.
///
/// SAFETY: `s` is NULL or a C string valid for the call.
pub(super) unsafe fn text(s: *const c_char) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: as the caller says.
    unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
}

/// `g_type_query`'s answer.
#[repr(C)]
pub(super) struct GTypeQuery {
    pub(super) ty: GType,
    pub(super) name: *const c_char,
    pub(super) class_size: c_uint,
    pub(super) instance_size: c_uint,
}

/// A `GError`.
#[repr(C)]
pub(super) struct GError {
    pub(super) domain: u32,
    pub(super) code: c_int,
    pub(super) message: *const c_char,
}

/// `GObjectClass` (GLib's `gobject/gobject.h`): the head of every class struct.
#[repr(C)]
struct GObjectClass {
    g_type: GType,
    construct_properties: P,
    functions: [P; 8],
    flags: usize,
    n_construct_properties: usize,
    pspecs: P,
    n_pspecs: usize,
    pdummy: [P; 3],
}

/// `GInputStreamClass` (GIO's `gio/ginputstream.h`): the page stream's class.
#[repr(C)]
pub(super) struct GInputStreamClass {
    parent: GObjectClass,
    pub(super) read_fn: P,
    pub(super) skip: P,
    pub(super) close_fn: P,
    pub(super) read_async: P,
    pub(super) read_finish: P,
    pub(super) skip_async: P,
    pub(super) skip_finish: P,
    pub(super) close_async: P,
    pub(super) close_finish: P,
    reserved: [P; 5],
}

/// `GInputStream`: a `GObject` and its private pointer.
#[repr(C)]
struct GInputStream {
    g_type_instance: P,
    ref_count: c_uint,
    qdata: P,
    private: P,
}

/// The page stream's instance: GLib's `GInputStream`, then what sopen keeps — the view and the
/// request it reads.
#[repr(C)]
pub(super) struct PageStream {
    parent: GInputStream,
    pub(super) view: u64,
    pub(super) request: u64,
}

/// `GInputStream`'s class and instance sizes as declared here: GLib's own (`g_type_query`) must
/// be these before the page stream's class is registered on them.
pub(super) fn stream_layout() -> (c_uint, c_uint) {
    (
        std::mem::size_of::<GInputStreamClass>() as c_uint,
        std::mem::size_of::<GInputStream>() as c_uint,
    )
}

/// The page stream's instance size: GLib's `GInputStream` and sopen's two fields.
pub(super) fn page_stream_size() -> c_uint {
    std::mem::size_of::<PageStream>() as c_uint
}

// GDK's event types and masks (`gdk/gdkevents.h`, `gdk/gdktypes.h`).
pub(super) const GDK_MOTION_NOTIFY: c_int = 3;
pub(super) const GDK_BUTTON_PRESS: c_int = 4;
pub(super) const GDK_BUTTON_RELEASE: c_int = 7;
pub(super) const GDK_KEY_PRESS: c_int = 8;
pub(super) const GDK_KEY_RELEASE: c_int = 9;
pub(super) const GDK_FOCUS_CHANGE: c_int = 12;
pub(super) const GDK_SCROLL: c_int = 31;
pub(super) const GDK_SCROLL_SMOOTH: c_int = 4;
pub(super) const GDK_SHIFT_MASK: c_uint = 1;
pub(super) const GDK_CONTROL_MASK: c_uint = 1 << 2;
pub(super) const GDK_MOD1_MASK: c_uint = 1 << 3;
pub(super) const GDK_BUTTON1_MASK: c_uint = 1 << 8;

/// `GdkEventMotion`.
#[repr(C)]
pub(super) struct EventMotion {
    pub(super) ty: c_int,
    pub(super) window: P,
    pub(super) send_event: i8,
    pub(super) time: u32,
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) axes: P,
    pub(super) state: c_uint,
    pub(super) is_hint: i16,
    pub(super) device: P,
    pub(super) x_root: f64,
    pub(super) y_root: f64,
}

/// `GdkEventButton`.
#[repr(C)]
pub(super) struct EventButton {
    pub(super) ty: c_int,
    pub(super) window: P,
    pub(super) send_event: i8,
    pub(super) time: u32,
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) axes: P,
    pub(super) state: c_uint,
    pub(super) button: c_uint,
    pub(super) device: P,
    pub(super) x_root: f64,
    pub(super) y_root: f64,
}

/// `GdkEventScroll`.
#[repr(C)]
pub(super) struct EventScroll {
    pub(super) ty: c_int,
    pub(super) window: P,
    pub(super) send_event: i8,
    pub(super) time: u32,
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) state: c_uint,
    pub(super) direction: c_int,
    pub(super) device: P,
    pub(super) x_root: f64,
    pub(super) y_root: f64,
    pub(super) delta_x: f64,
    pub(super) delta_y: f64,
    pub(super) is_stop: c_uint,
}

/// `GdkEventKey`.
#[repr(C)]
pub(super) struct EventKey {
    pub(super) ty: c_int,
    pub(super) window: P,
    pub(super) send_event: i8,
    pub(super) time: u32,
    pub(super) state: c_uint,
    pub(super) keyval: c_uint,
    pub(super) length: c_int,
    pub(super) string: *mut c_char,
    pub(super) hardware_keycode: u16,
    pub(super) group: u8,
    pub(super) is_modifier: c_uint,
}

/// `GdkEventFocus`.
#[repr(C)]
pub(super) struct EventFocus {
    pub(super) ty: c_int,
    pub(super) window: P,
    pub(super) send_event: i8,
    pub(super) focus_in: i16,
}

// WebKit's enums (`webkit/WebKitWebView.h`, `WebKitSettings.h`, `WebKitError.h`, …).
pub(super) const WEBKIT_LOAD_STARTED: c_int = 0;
pub(super) const WEBKIT_LOAD_COMMITTED: c_int = 2;
pub(super) const WEBKIT_LOAD_FINISHED: c_int = 3;
pub(super) const WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION: c_int = 1;
pub(super) const WEBKIT_POLICY_DECISION_TYPE_RESPONSE: c_int = 2;
pub(super) const WEBKIT_NAVIGATION_TYPE_LINK_CLICKED: c_int = 0;
pub(super) const WEBKIT_HARDWARE_ACCELERATION_POLICY_NEVER: c_int = 2;
pub(super) const WEBKIT_CACHE_MODEL_DOCUMENT_VIEWER: c_int = 0;
pub(super) const WEBKIT_NETWORK_PROXY_MODE_CUSTOM: c_int = 2;
pub(super) const SOUP_MESSAGE_HEADERS_RESPONSE: c_int = 1;
pub(super) const G_IO_ERROR_FAILED: c_int = 0;
pub(super) const G_IO_ERROR_CANCELLED: c_int = 19;
pub(super) const G_PRIORITY_DEFAULT: c_int = 0;
