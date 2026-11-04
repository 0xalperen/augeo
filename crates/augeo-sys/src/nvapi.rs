//! NVIDIA driver settings (DRS) through `nvapi64.dll`.
//!
//! The DLL ships with the driver and is loaded from System32 only. Functions are looked up by the IDs
//! in NVIDIA's public, MIT-licensed `nvapi_interface.h`. A missing driver isn't an error: augeo then
//! disables NVIDIA tweaks and says why.
//!
//! Struct layouts match `nvapi.h`. `cargo xtask check-nvapi-layout` compares them with a real C
//! compiler in CI, because no CI runner has a GPU to catch a mistake here at run time.

use std::{
    cell::OnceCell,
    collections::BTreeSet,
    ffi::c_void,
    mem::{offset_of, size_of},
    path::{Path, PathBuf},
    ptr::null_mut,
};

use augeo_ports::{DrsProfile, PortError, Value};
use windows::{
    Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
    core::{HSTRING, s},
};

const UNICODE_MAX: usize = 2048;
const BINARY_MAX: usize = 4096;
type WStr = [u16; UNICODE_MAX];
type Handle = *mut c_void;

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct BinarySetting {
    value_length: u32,
    value_data: [u8; BINARY_MAX],
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
union SettingValue {
    dword: u32,
    binary: BinarySetting,
    wide: WStr,
    qword: u64,
}

#[repr(C, packed(4))]
struct Setting {
    version: u32,
    name: WStr,
    id: u32,
    kind: u32,
    location: u32,
    is_current_predefined: u32,
    is_predefined_valid: u32,
    predefined: SettingValue,
    current: SettingValue,
}

#[repr(C)]
struct Profile {
    version: u32,
    name: WStr,
    gpu_support: u32,
    is_predefined: u32,
    num_apps: u32,
    num_settings: u32,
}

#[repr(C)]
struct Application {
    version: u32,
    is_predefined: u32,
    app_name: WStr,
    friendly_name: WStr,
    launcher: WStr,
    file_in_folder: WStr,
    flags: u32,
    command_line: WStr,
}

// These numbers are also checked against nvapi.h by `cargo xtask check-nvapi-layout`.
const _: () = assert!(size_of::<Setting>() == 12320);
const _: () = assert!(offset_of!(Setting, id) == 4100);
const _: () = assert!(offset_of!(Setting, location) == 4108);
const _: () = assert!(offset_of!(Setting, current) == 8220);
const _: () = assert!(size_of::<Profile>() == 4116);
const _: () = assert!(offset_of!(Profile, num_settings) == 4112);
const _: () = assert!(size_of::<Application>() == 20492);
const _: () = assert!(offset_of!(Application, command_line) == 16396);

const SETTING_VER: u32 = size_of::<Setting>() as u32 | (1 << 16);
const PROFILE_VER: u32 = size_of::<Profile>() as u32 | (1 << 16);
const APPLICATION_VER: u32 = size_of::<Application>() as u32 | (4 << 16);

const INITIALIZE: u32 = 0x0150_E828;
const CREATE_SESSION: u32 = 0x0694_D52E;
const DESTROY_SESSION: u32 = 0xDAD9_CFF8;
const LOAD_SETTINGS: u32 = 0x375D_BD6B;
const SAVE_SETTINGS: u32 = 0xFCBC_7E14;
const SAVE_TO_FILE: u32 = 0x2BE2_5DF8;
const CREATE_PROFILE: u32 = 0xCC17_6068;
const DELETE_PROFILE: u32 = 0x1709_3206;
const GET_PROFILE_INFO: u32 = 0x61CD_6FD6;
#[cfg(all(test, feature = "gpu-tests"))]
const FIND_PROFILE: u32 = 0x7E4A_9A0B;
const FIND_APP: u32 = 0xEEE5_66B2;
const CREATE_APP: u32 = 0x4347_A9DE;
const SET_SETTING: u32 = 0x577D_D202;
const GET_SETTING: u32 = 0x73BF_8338;
const DELETE_SETTING: u32 = 0xE4A2_6362;
const GET_BASE: u32 = 0xDA84_66A0;
const ENUM_AVAILABLE_IDS: u32 = 0xF020_614A;

const OK: i32 = 0;
const SETTING_NOT_FOUND: i32 = -160;
#[cfg(all(test, feature = "gpu-tests"))]
const PROFILE_NOT_FOUND: i32 = -163;
const EXECUTABLE_NOT_FOUND: i32 = -166;
const NEEDS_ADMIN: i32 = -137;

const CURRENT_PROFILE_LOCATION: u32 = 0;
const DWORD_TYPE: u32 = 0;

/// Marks a profile as one augeo created, so undo may remove it once it is empty.
const OWN_PROFILE_PREFIX: &str = "augeo ";

fn check(what: &str, status: i32) -> Result<(), PortError> {
    match status {
        OK => Ok(()),
        NEEDS_ADMIN => Err(PortError::Denied(format!(
            "{what} needs administrator rights"
        ))),
        other => Err(PortError::Failed(format!(
            "{what} failed with NVAPI status {other}"
        ))),
    }
}

fn to_wstr(text: &str) -> WStr {
    let mut out = [0u16; UNICODE_MAX];
    for (slot, unit) in out
        .iter_mut()
        .take(UNICODE_MAX - 1)
        .zip(text.encode_utf16())
    {
        *slot = unit;
    }
    out
}

fn from_wstr(text: &WStr) -> String {
    let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..end])
}

/// Allocates a zeroed value on the heap. These structs are too large to want on the stack.
fn boxed_zeroed<T>() -> Box<T> {
    // SAFETY: every struct used here is plain integers and arrays, for which all zero bits are valid.
    Box::new(unsafe { std::mem::zeroed() })
}

/// The loaded NVAPI library.
pub(crate) struct Drs {
    query: unsafe extern "C" fn(u32) -> *mut c_void,
    available: OnceCell<BTreeSet<u32>>,
    backup: Option<PathBuf>,
    backed_up: bool,
}

impl std::fmt::Debug for Drs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Drs")
    }
}

macro_rules! call {
    ($self:expr, $id:expr, ($($ty:ty),*), $($arg:expr),*) => {{
        let pointer = $self.function($id)?;
        // SAFETY: the signature matches the declaration in nvapi.h for this function ID.
        let function: unsafe extern "C" fn($($ty),*) -> i32 =
            unsafe { std::mem::transmute(pointer) };
        // SAFETY: every pointer argument refers to a live local that outlives the call.
        unsafe { function($($arg),*) }
    }};
}

/// One open DRS session. A session sees a snapshot of settings, so each operation uses a fresh one.
struct Session<'a> {
    drs: &'a Drs,
    handle: Handle,
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        let drs = self.drs;
        let handle = self.handle;
        let close = || -> Result<(), PortError> {
            let _ = call!(drs, DESTROY_SESSION, (Handle), handle);
            Ok(())
        };
        let _ = close();
    }
}

impl Drs {
    /// Loads NVAPI, or returns `None` when no NVIDIA driver is present.
    pub(crate) fn load(backup: Option<PathBuf>) -> Option<Self> {
        let name = HSTRING::from("nvapi64.dll");
        // SAFETY: the name outlives the call, and the search is limited to System32.
        let module = unsafe { LoadLibraryExW(&name, None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.ok()?;
        // SAFETY: the module just loaded, and the name is a valid constant.
        let pointer = unsafe { GetProcAddress(module, s!("nvapi_QueryInterface")) }?;
        // SAFETY: this is the signature NVAPI documents for its one exported function.
        let query = unsafe {
            std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "C" fn(u32) -> *mut c_void,
            >(pointer)
        };
        let drs = Self {
            query,
            available: OnceCell::new(),
            backup,
            backed_up: false,
        };
        let status = (|| -> Result<i32, PortError> { Ok(call!(drs, INITIALIZE, (),)) })().ok()?;
        (status == OK).then_some(drs)
    }

    fn function(&self, id: u32) -> Result<*mut c_void, PortError> {
        // SAFETY: the query function takes a plain integer, and returns a pointer or null.
        let pointer = unsafe { (self.query)(id) };
        if pointer.is_null() {
            Err(PortError::NotAvailable(format!(
                "this NVIDIA driver has no function 0x{id:08X}"
            )))
        } else {
            Ok(pointer)
        }
    }

    fn session(&self) -> Result<Session<'_>, PortError> {
        let mut handle: Handle = null_mut();
        check(
            "opening driver settings",
            call!(self, CREATE_SESSION, (*mut Handle), &mut handle),
        )?;
        let session = Session { drs: self, handle };
        check(
            "loading driver settings",
            call!(self, LOAD_SETTINGS, (Handle), session.handle),
        )?;
        Ok(session)
    }

    fn base_profile(&self, session: &Session<'_>) -> Result<Handle, PortError> {
        let mut profile: Handle = null_mut();
        check(
            "opening the global profile",
            call!(
                self,
                GET_BASE,
                (Handle, *mut Handle),
                session.handle,
                &mut profile
            ),
        )?;
        Ok(profile)
    }

    /// The profile that applies to a game, if the driver has one for it.
    fn game_profile(&self, session: &Session<'_>, exe: &str) -> Result<Option<Handle>, PortError> {
        let name = to_wstr(exe);
        let mut profile: Handle = null_mut();
        let mut app = boxed_zeroed::<Application>();
        app.version = APPLICATION_VER;
        let status = call!(
            self,
            FIND_APP,
            (Handle, *const WStr, *mut Handle, *mut Application),
            session.handle,
            &name,
            &mut profile,
            &mut *app
        );
        match status {
            OK => Ok(Some(profile)),
            EXECUTABLE_NOT_FOUND => Ok(None),
            other => check(&format!("finding the profile for {exe}"), other).map(|()| None),
        }
    }

    fn create_game_profile(&self, session: &Session<'_>, exe: &str) -> Result<Handle, PortError> {
        let what = format!("creating a profile for {exe}");
        let mut info = boxed_zeroed::<Profile>();
        info.version = PROFILE_VER;
        info.name = to_wstr(&format!("{OWN_PROFILE_PREFIX}{exe}"));
        let mut handle: Handle = null_mut();
        check(
            &what,
            call!(
                self,
                CREATE_PROFILE,
                (Handle, *const Profile, *mut Handle),
                session.handle,
                &*info,
                &mut handle
            ),
        )?;
        let mut app = boxed_zeroed::<Application>();
        app.version = APPLICATION_VER;
        app.app_name = to_wstr(exe);
        check(
            &what,
            call!(
                self,
                CREATE_APP,
                (Handle, Handle, *mut Application),
                session.handle,
                handle,
                &mut *app
            ),
        )?;
        Ok(handle)
    }

    fn available_ids(&self) -> Result<&BTreeSet<u32>, PortError> {
        if let Some(ids) = self.available.get() {
            return Ok(ids);
        }
        let mut ids = vec![0u32; 8192];
        let mut count = ids.len() as u32;
        check(
            "listing driver settings",
            call!(
                self,
                ENUM_AVAILABLE_IDS,
                (*mut u32, *mut u32),
                ids.as_mut_ptr(),
                &mut count
            ),
        )?;
        ids.truncate(count as usize);
        Ok(self.available.get_or_init(|| ids.into_iter().collect()))
    }

    /// Whether this driver accepts a setting ID. Community-known IDs can vanish in any driver.
    pub(crate) fn is_available(&self, setting: u32) -> Result<bool, PortError> {
        Ok(self.available_ids()?.contains(&setting))
    }

    /// Reads what is stored in this exact profile, ignoring values inherited from elsewhere.
    pub(crate) fn read(&self, profile: &DrsProfile, setting: u32) -> Result<Value, PortError> {
        let session = self.session()?;
        let handle = match profile {
            DrsProfile::Base => self.base_profile(&session)?,
            DrsProfile::Game { exe } => match self.game_profile(&session, exe)? {
                Some(handle) => handle,
                None => return Ok(Value::Absent),
            },
        };
        let mut out = boxed_zeroed::<Setting>();
        out.version = SETTING_VER;
        let status = call!(
            self,
            GET_SETTING,
            (Handle, Handle, u32, *mut Setting),
            session.handle,
            handle,
            setting,
            &mut *out
        );
        match status {
            OK => {}
            SETTING_NOT_FOUND => return Ok(Value::Absent),
            other => check(&format!("reading setting 0x{setting:08X}"), other)?,
        }
        if out.location != CURRENT_PROFILE_LOCATION {
            return Ok(Value::Absent);
        }
        if { out.kind } != DWORD_TYPE {
            let kind = out.kind;
            return Err(PortError::Failed(format!(
                "setting 0x{setting:08X} has type {kind}, which augeo doesn't handle"
            )));
        }
        // SAFETY: the type check above says the driver filled the DWORD member of the union.
        Ok(Value::Dword(unsafe { out.current.dword }))
    }

    /// Exports the whole driver database once per process, before the first change.
    fn back_up_once(&mut self) -> Result<(), PortError> {
        if self.backed_up {
            return Ok(());
        }
        if let Some(path) = self.backup.clone() {
            export(self, &path)?;
        }
        self.backed_up = true;
        Ok(())
    }

    pub(crate) fn write(
        &mut self,
        profile: &DrsProfile,
        setting: u32,
        value: &Value,
    ) -> Result<(), PortError> {
        self.back_up_once()?;
        let session = self.session()?;
        match value {
            Value::Dword(number) => {
                let handle = match profile {
                    DrsProfile::Base => self.base_profile(&session)?,
                    DrsProfile::Game { exe } => match self.game_profile(&session, exe)? {
                        Some(handle) => handle,
                        None => self.create_game_profile(&session, exe)?,
                    },
                };
                let mut new = boxed_zeroed::<Setting>();
                new.version = SETTING_VER;
                new.id = setting;
                new.kind = DWORD_TYPE;
                new.current.dword = *number;
                check(
                    &format!("setting 0x{setting:08X}"),
                    call!(
                        self,
                        SET_SETTING,
                        (Handle, Handle, *mut Setting),
                        session.handle,
                        handle,
                        &mut *new
                    ),
                )?;
            }
            Value::Absent => {
                let handle = match profile {
                    DrsProfile::Base => Some(self.base_profile(&session)?),
                    DrsProfile::Game { exe } => self.game_profile(&session, exe)?,
                };
                let Some(handle) = handle else {
                    return Ok(());
                };
                let status = call!(
                    self,
                    DELETE_SETTING,
                    (Handle, Handle, u32),
                    session.handle,
                    handle,
                    setting
                );
                if status != SETTING_NOT_FOUND {
                    check(&format!("removing setting 0x{setting:08X}"), status)?;
                }
                if let DrsProfile::Game { .. } = profile {
                    self.remove_if_own_and_empty(&session, handle)?;
                }
            }
            other => {
                return Err(PortError::Failed(format!(
                    "NVIDIA setting 0x{setting:08X} can't be set to {other}"
                )));
            }
        }
        check(
            "saving driver settings",
            call!(self, SAVE_SETTINGS, (Handle), session.handle),
        )
    }

    /// Removes a profile only if augeo created it and nothing is left in it.
    fn remove_if_own_and_empty(
        &self,
        session: &Session<'_>,
        handle: Handle,
    ) -> Result<(), PortError> {
        let mut info = boxed_zeroed::<Profile>();
        info.version = PROFILE_VER;
        check(
            "reading a profile",
            call!(
                self,
                GET_PROFILE_INFO,
                (Handle, Handle, *mut Profile),
                session.handle,
                handle,
                &mut *info
            ),
        )?;
        let own = from_wstr(&info.name).starts_with(OWN_PROFILE_PREFIX);
        if own && info.num_settings == 0 && info.is_predefined == 0 {
            check(
                "removing an empty profile",
                call!(
                    self,
                    DELETE_PROFILE,
                    (Handle, Handle),
                    session.handle,
                    handle
                ),
            )?;
        }
        Ok(())
    }

    /// Whether a profile with this exact name exists. Used by tests to check for leftovers.
    #[cfg(all(test, feature = "gpu-tests"))]
    pub(crate) fn profile_exists(&self, name: &str) -> Result<bool, PortError> {
        let session = self.session()?;
        let wide = to_wstr(name);
        let mut handle: Handle = null_mut();
        let status = call!(
            self,
            FIND_PROFILE,
            (Handle, *const WStr, *mut Handle),
            session.handle,
            &wide,
            &mut handle
        );
        match status {
            OK => Ok(true),
            PROFILE_NOT_FOUND => Ok(false),
            other => check("finding a profile", other).map(|()| false),
        }
    }
}

fn export(drs: &Drs, path: &Path) -> Result<(), PortError> {
    let session = drs.session()?;
    let text = path
        .to_str()
        .ok_or_else(|| PortError::Failed("the backup path isn't valid text".into()))?;
    let file = to_wstr(text);
    check(
        "backing up driver settings",
        call!(
            drs,
            SAVE_TO_FILE,
            (Handle, *const WStr),
            session.handle,
            &file
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_match_nvapi_macros() {
        assert_eq!(SETTING_VER, 0x0001_3020);
        assert_eq!(PROFILE_VER, 0x0001_1014);
        assert_eq!(APPLICATION_VER, 0x0004_500C);
    }

    #[test]
    fn wide_strings_round_trip_and_stay_terminated() {
        assert_eq!(from_wstr(&to_wstr("cs2.exe")), "cs2.exe");
        let long = "x".repeat(UNICODE_MAX + 50);
        let wide = to_wstr(&long);
        assert_eq!(wide[UNICODE_MAX - 1], 0);
        assert_eq!(from_wstr(&wide).len(), UNICODE_MAX - 1);
    }

    #[test]
    fn a_failed_status_is_reported() {
        assert!(check("x", OK).is_ok());
        assert!(matches!(check("x", NEEDS_ADMIN), Err(PortError::Denied(_))));
        assert!(matches!(check("x", -1), Err(PortError::Failed(_))));
    }

    /// Needs a real NVIDIA GPU. Only ever touches a throwaway profile for a fake exe.
    #[cfg(feature = "gpu-tests")]
    mod gpu {
        use super::*;

        const FRL_FPS: u32 = 0x1083_5002;
        const SCRATCH_EXE: &str = "augeo-scratch-test.exe";

        fn drs(backup: Option<PathBuf>) -> Drs {
            Drs::load(backup).expect("these tests need an NVIDIA driver")
        }

        fn scratch() -> DrsProfile {
            DrsProfile::Game {
                exe: SCRATCH_EXE.into(),
            }
        }

        #[test]
        fn a_game_setting_round_trips_and_leaves_nothing_behind() {
            let backup = std::env::temp_dir().join("augeo-gpu-test-backup.bin");
            let _ = std::fs::remove_file(&backup);
            let mut drs = drs(Some(backup.clone()));
            let own_name = format!("{OWN_PROFILE_PREFIX}{SCRATCH_EXE}");

            assert_eq!(drs.read(&scratch(), FRL_FPS).unwrap(), Value::Absent);
            assert!(!drs.profile_exists(&own_name).unwrap());

            drs.write(&scratch(), FRL_FPS, &Value::Dword(123)).unwrap();
            assert!(
                backup.exists(),
                "the database must be exported before the first change"
            );
            assert!(std::fs::metadata(&backup).unwrap().len() > 0);
            assert!(drs.profile_exists(&own_name).unwrap());
            // Each read uses a fresh session, so this proves the value reached the database.
            assert_eq!(drs.read(&scratch(), FRL_FPS).unwrap(), Value::Dword(123));

            drs.write(&scratch(), FRL_FPS, &Value::Dword(77)).unwrap();
            assert_eq!(drs.read(&scratch(), FRL_FPS).unwrap(), Value::Dword(77));

            drs.write(&scratch(), FRL_FPS, &Value::Absent).unwrap();
            assert_eq!(drs.read(&scratch(), FRL_FPS).unwrap(), Value::Absent);
            assert!(!drs.profile_exists(&own_name).unwrap());

            // Removing what is already gone is safe.
            drs.write(&scratch(), FRL_FPS, &Value::Absent).unwrap();
            let _ = std::fs::remove_file(&backup);
        }

        #[test]
        fn the_driver_lists_public_settings() {
            let drs = drs(None);
            for id in [0x007B_A09E, 0x1057_EB71, 0x00A8_79CF, FRL_FPS, 0x00AC_8497] {
                assert!(drs.is_available(id).unwrap(), "0x{id:08X}");
            }
            assert!(!drs.is_available(0xDEAD_BEEF).unwrap());
        }

        #[test]
        fn reading_the_global_profile_works() {
            let drs = drs(None);
            let value = drs.read(&DrsProfile::Base, FRL_FPS).unwrap();
            assert!(
                matches!(value, Value::Absent | Value::Dword(_)),
                "{value:?}"
            );
        }

        #[test]
        fn a_game_with_no_profile_reads_as_absent() {
            let drs = drs(None);
            let profile = DrsProfile::Game {
                exe: "augeo-no-such-game.exe".into(),
            };
            assert_eq!(drs.read(&profile, FRL_FPS).unwrap(), Value::Absent);
        }
    }
}
