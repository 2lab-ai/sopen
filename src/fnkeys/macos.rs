//! The real [`Switch`] on macOS (04 §6.2): `HIDFKeyMode` through IOKit, loaded at runtime
//! (`dlopen`, nothing linked). Set: `IOServiceOpen(IOService:/IOResources/IOHIDSystem,
//! kIOHIDParamConnectType)` then `IOHIDSetCFTypeParameter(connect, "HIDFKeyMode", n)`; read: the
//! registry property `HIDParameters["HIDFKeyMode"]` of the same entry. No entitlement,
//! Accessibility grant or root is needed (measured, E1 in 02 §8.2). Nothing is loaded or opened
//! before the first read or write; the connection is opened at the first write.
//!
//! Prototypes as the macOS SDK declares them (`IOKitLib.h`, `IOHIDLib.h`, `CFNumber.h`,
//! `CFDictionary.h`, `CFString.h`): mach ports and IOKit objects are `unsigned int`,
//! `kern_return_t` is `int`, `CFIndex` / `CFTypeID` are `long` / `unsigned long`, `Boolean` is
//! `unsigned char`.

use super::Switch;
use anyhow::{bail, Context, Result};
use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr};

type MachPort = c_uint;
type KernReturn = c_int;
type CfType = *const c_void;
type CfString = *const c_void;
type CfAllocator = *const c_void;

const IOKIT: &CStr = c"/System/Library/Frameworks/IOKit.framework/IOKit";
const CORE_FOUNDATION: &CStr =
    c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";
const IOHIDSYSTEM: &CStr = c"IOService:/IOResources/IOHIDSystem";
/// `kIOMainPortDefault`.
const MAIN_PORT_DEFAULT: MachPort = 0;
/// `kIOHIDParamConnectType` (`IOHIDShared.h`).
const PARAM_CONNECT_TYPE: u32 = 1;
/// `kCFNumberSInt32Type`.
const SINT32: c_long = 3;
/// `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;

/// `HIDFKeyMode` through IOKit. Loaded and opened on first use.
#[derive(Default)]
pub struct IoHid {
    api: Option<Api>,
}

// SAFETY: what `Api` holds is usable from any thread: the library handles and function pointers
// are process-wide, the two CFStrings are immutable, and mach port names are integers valid in
// the whole task.
unsafe impl Send for IoHid {}

impl IoHid {
    pub fn new() -> Self {
        Self::default()
    }

    fn api(&mut self) -> Result<&mut Api> {
        if self.api.is_none() {
            self.api = Some(Api::load()?);
        }
        Ok(self.api.as_mut().expect("loaded just now"))
    }
}

impl Switch for IoHid {
    fn read(&mut self) -> Result<i64> {
        self.api()?.read()
    }

    fn write(&mut self, value: i64) -> Result<()> {
        self.api()?.write(value)
    }
}

struct Api {
    io_registry_entry_from_path: unsafe extern "C" fn(MachPort, *const c_char) -> MachPort,
    io_service_open: unsafe extern "C" fn(MachPort, MachPort, u32, *mut MachPort) -> KernReturn,
    io_service_close: unsafe extern "C" fn(MachPort) -> KernReturn,
    io_object_release: unsafe extern "C" fn(MachPort) -> KernReturn,
    io_registry_entry_create_cf_property:
        unsafe extern "C" fn(MachPort, CfString, CfAllocator, u32) -> CfType,
    io_hid_set_cf_type_parameter: unsafe extern "C" fn(MachPort, CfString, CfType) -> KernReturn,
    cf_string_create_with_c_string:
        unsafe extern "C" fn(CfAllocator, *const c_char, u32) -> CfString,
    cf_number_create: unsafe extern "C" fn(CfAllocator, c_long, *const c_void) -> CfType,
    cf_number_get_value: unsafe extern "C" fn(CfType, c_long, *mut c_void) -> u8,
    cf_get_type_id: unsafe extern "C" fn(CfType) -> c_ulong,
    cf_number_get_type_id: unsafe extern "C" fn() -> c_ulong,
    cf_dictionary_get_type_id: unsafe extern "C" fn() -> c_ulong,
    cf_dictionary_get_value: unsafe extern "C" fn(CfType, *const c_void) -> *const c_void,
    cf_release: unsafe extern "C" fn(CfType),
    /// `"HIDFKeyMode"` and `"HIDParameters"`.
    key: CfString,
    parameters: CfString,
    /// IOHIDSystem's registry entry.
    system: MachPort,
    /// The param connection, 0 until the first write.
    connect: MachPort,
}

/// Opens `path` with `dlopen`; the handle is never closed (the frameworks stay loaded, as
/// they are for any process that uses them).
fn open(path: &CStr) -> Result<*mut c_void> {
    // SAFETY: `path` is a C string; these are Apple's system frameworks.
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        bail!("cannot load {}", path.to_string_lossy());
    }
    Ok(handle)
}

/// The symbol `name` of `handle`, as the function type `F`.
///
/// SAFETY: the caller names `F` as the SDK declares the symbol.
unsafe fn symbol<F: Copy>(handle: *mut c_void, name: &CStr) -> Result<F> {
    // SAFETY: `handle` came from dlopen, `name` is a C string.
    let found = unsafe { libc::dlsym(handle, name.as_ptr()) };
    if found.is_null() {
        bail!("IOKit has no {}", name.to_string_lossy());
    }
    debug_assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*mut c_void>());
    // SAFETY: a function pointer the size of a data pointer; the caller vouches for its type.
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&found) })
}

impl Api {
    fn load() -> Result<Api> {
        let iokit = open(IOKIT)?;
        let cf = open(CORE_FOUNDATION)?;
        // SAFETY: every type below is the SDK's prototype of the symbol it names.
        let mut api = unsafe {
            Api {
                io_registry_entry_from_path: symbol(iokit, c"IORegistryEntryFromPath")?,
                io_service_open: symbol(iokit, c"IOServiceOpen")?,
                io_service_close: symbol(iokit, c"IOServiceClose")?,
                io_object_release: symbol(iokit, c"IOObjectRelease")?,
                io_registry_entry_create_cf_property: symbol(
                    iokit,
                    c"IORegistryEntryCreateCFProperty",
                )?,
                io_hid_set_cf_type_parameter: symbol(iokit, c"IOHIDSetCFTypeParameter")?,
                cf_string_create_with_c_string: symbol(cf, c"CFStringCreateWithCString")?,
                cf_number_create: symbol(cf, c"CFNumberCreate")?,
                cf_number_get_value: symbol(cf, c"CFNumberGetValue")?,
                cf_get_type_id: symbol(cf, c"CFGetTypeID")?,
                cf_number_get_type_id: symbol(cf, c"CFNumberGetTypeID")?,
                cf_dictionary_get_type_id: symbol(cf, c"CFDictionaryGetTypeID")?,
                cf_dictionary_get_value: symbol(cf, c"CFDictionaryGetValue")?,
                cf_release: symbol(cf, c"CFRelease")?,
                key: std::ptr::null(),
                parameters: std::ptr::null(),
                system: 0,
                connect: 0,
            }
        };
        // From here `Drop` frees what is set.
        // SAFETY: C strings; a null allocator is the default one.
        unsafe {
            api.key = (api.cf_string_create_with_c_string)(
                std::ptr::null(),
                c"HIDFKeyMode".as_ptr(),
                UTF8,
            );
            api.parameters = (api.cf_string_create_with_c_string)(
                std::ptr::null(),
                c"HIDParameters".as_ptr(),
                UTF8,
            );
            api.system = (api.io_registry_entry_from_path)(MAIN_PORT_DEFAULT, IOHIDSYSTEM.as_ptr());
        }
        if api.key.is_null() || api.parameters.is_null() {
            bail!("cannot make the CoreFoundation strings");
        }
        if api.system == 0 {
            bail!("no {} in the I/O registry", IOHIDSYSTEM.to_string_lossy());
        }
        Ok(api)
    }

    /// `HIDParameters["HIDFKeyMode"]` of IOHIDSystem's registry entry.
    fn read(&mut self) -> Result<i64> {
        // SAFETY: `system` and the keys are live (see `load`); the property is released below.
        let parameters = unsafe {
            (self.io_registry_entry_create_cf_property)(
                self.system,
                self.parameters,
                std::ptr::null(),
                0,
            )
        };
        if parameters.is_null() {
            bail!("IOHIDSystem has no HIDParameters");
        }
        let value = self.number_in(parameters);
        // SAFETY: a Create function's result, released once.
        unsafe { (self.cf_release)(parameters) };
        value
    }

    /// `dict[HIDFKeyMode]` as a number.
    fn number_in(&self, dict: CfType) -> Result<i64> {
        // SAFETY: `dict` is a live CF object; the value it gives is borrowed, not released.
        unsafe {
            if (self.cf_get_type_id)(dict) != (self.cf_dictionary_get_type_id)() {
                bail!("HIDParameters is not a dictionary");
            }
            let value = (self.cf_dictionary_get_value)(dict, self.key);
            if value.is_null() {
                bail!("HIDParameters has no HIDFKeyMode");
            }
            if (self.cf_get_type_id)(value) != (self.cf_number_get_type_id)() {
                bail!("HIDFKeyMode is not a number");
            }
            let mut n: i32 = 0;
            if (self.cf_number_get_value)(value, SINT32, (&mut n as *mut i32).cast()) == 0 {
                bail!("HIDFKeyMode is not a 32-bit number");
            }
            Ok(i64::from(n))
        }
    }

    /// `IOHIDSetCFTypeParameter(connect, "HIDFKeyMode", value)`, opening the connection first.
    fn write(&mut self, value: i64) -> Result<()> {
        let n = i32::try_from(value).context("HIDFKeyMode out of range")?;
        if self.connect == 0 {
            // SAFETY: mach_task_self() is the task's own send right, read from the variable the
            // macro reads; `connect` receives the new connection.
            #[allow(deprecated)]
            let task = unsafe { libc::mach_task_self_ };
            let mut connect: MachPort = 0;
            let kr = unsafe {
                (self.io_service_open)(self.system, task, PARAM_CONNECT_TYPE, &mut connect)
            };
            if kr != 0 {
                bail!("IOServiceOpen(IOHIDSystem) returned {kr:#x}");
            }
            self.connect = connect;
        }
        // SAFETY: a CFNumber made from a live i32, released once after the call.
        unsafe {
            let number =
                (self.cf_number_create)(std::ptr::null(), SINT32, (&n as *const i32).cast());
            if number.is_null() {
                bail!("cannot make a CFNumber");
            }
            let kr = (self.io_hid_set_cf_type_parameter)(self.connect, self.key, number);
            (self.cf_release)(number);
            if kr != 0 {
                bail!("IOHIDSetCFTypeParameter(HIDFKeyMode, {n}) returned {kr:#x}");
            }
        }
        Ok(())
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        // SAFETY: each is released once, only when it was made.
        unsafe {
            if self.connect != 0 {
                (self.io_service_close)(self.connect);
            }
            if self.system != 0 {
                (self.io_object_release)(self.system);
            }
            if !self.key.is_null() {
                (self.cf_release)(self.key);
            }
            if !self.parameters.is_null() {
                (self.cf_release)(self.parameters);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fnkeys::{acquire, release, Acquired, Holder, Machine, OFF, ON};

    /// The one test of the real switch: run by hand on a Mac, never by the gate. It changes the
    /// Mac's global `HIDFKeyMode` for a few milliseconds, only from the Mac's default 0, and puts
    /// 0 back on every way out (a failed assertion included):
    /// `cargo test --lib fnkeys::macos::tests::the_real_switch_sets_reads_back_and_restores -- --ignored --exact`.
    /// First a write of the value it already has goes through the whole path, the connection
    /// included (no exposure); then sopen's own acquire and release, against a temp state folder.
    #[test]
    #[ignore = "changes the Mac's global HIDFKeyMode for milliseconds; run by hand with --ignored --exact"]
    fn the_real_switch_sets_reads_back_and_restores() {
        let mut hid = IoHid::new();
        let before = hid.read().expect("read HIDFKeyMode");
        assert_eq!(
            before, OFF,
            "HIDFKeyMode is {before}: the owner's own setting, not touched"
        );
        hid.write(OFF).expect("write the value it has");
        assert_eq!(hid.read().unwrap(), OFF, "after writing the value it has");

        /// Puts 0 back however the window ends.
        struct Restore(IoHid);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Err(e) = self.0.write(OFF) {
                    eprintln!("RESTORE FAILED: {e:#}");
                }
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let me = Holder::this_process().unwrap();
        let machine = Machine::this();
        let mut guard = Restore(hid);
        let got = acquire(tmp.path(), &mut guard.0, me, &machine);
        let during = guard.0.read();
        let released = release(tmp.path(), &mut guard.0, me, &machine);
        let after = guard.0.read();
        drop(guard);
        let mut hid = IoHid::new();
        let last = hid.read().expect("read after the window");

        assert_eq!(last, OFF, "HIDFKeyMode is back at 0");
        assert_eq!(got.unwrap(), Acquired::Set);
        assert_eq!(during.unwrap(), ON, "read back during the window");
        released.unwrap();
        assert_eq!(after.unwrap(), OFF, "after sopen's release");
        assert!(!tmp.path().join(crate::fnkeys::STATE).exists());
    }
}
