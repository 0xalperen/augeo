//! The state directory, `%ProgramData%\augeo`.
//!
//! A new folder under ProgramData inherits a rule that lets any user create files, which would let an
//! ordinary program tamper with what an elevated augeo reads and runs. So the folder gets its own
//! protected permissions at creation, and every start checks them. These permissions are the security
//! boundary for augeo's state: nothing else is.

use std::{
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
};

use augeo_ports::PortError;
use windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree},
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorDacl,
            GetSecurityDescriptorOwner, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
            SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT},
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
    },
    core::{PCWSTR, PWSTR},
};

use crate::util::wide;

/// Owner Administrators; protected; SYSTEM and Administrators have full control; Users can read.
const SDDL: &str = "O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";

const SID_SYSTEM: &str = "S-1-5-18";
const SID_ADMINISTRATORS: &str = "S-1-5-32-544";
const SID_TRUSTED_INSTALLER: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const INHERIT_ONLY_ACE: u8 = 0x08;

/// Rights that would let someone change the folder or what's in it.
const WRITE_RIGHTS: u32 = 0x0000_0002 // create files / write data
    | 0x0000_0004 // create folders / append data
    | 0x0000_0040 // delete child
    | 0x0001_0000 // delete
    | 0x0004_0000 // change permissions
    | 0x0008_0000 // take ownership
    | 0x1000_0000 // generic all
    | 0x4000_0000; // generic write

/// Why a directory can't be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Untrusted {
    Reparse,
    Owner(String),
    WritableBy(String),
    NoDacl,
}

impl std::fmt::Display for Untrusted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Untrusted::Reparse => f.write_str("it is a link to somewhere else"),
            Untrusted::Owner(sid) => write!(f, "it is owned by {sid}"),
            Untrusted::WritableBy(sid) => write!(f, "{sid} can write to it"),
            Untrusted::NoDacl => {
                f.write_str("it has no permissions set, so anyone can write to it")
            }
        }
    }
}

struct LocalMemory(*mut core::ffi::c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this memory came from an API that says to release it with LocalFree.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0)));
            }
        }
    }
}

fn sid_text(sid: PSID) -> Result<String, PortError> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` points into a live security descriptor, and `text` is a valid place for the result.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }
        .map_err(|e| PortError::Failed(format!("reading a SID: {e}")))?;
    let _guard = LocalMemory(text.0.cast());
    // SAFETY: the API succeeded, so this is a terminated string until the guard frees it.
    unsafe { text.to_string() }.map_err(|e| PortError::Failed(format!("reading a SID: {e}")))
}

/// Checks owner and permissions in a security descriptor.
fn analyse(descriptor: PSECURITY_DESCRIPTOR) -> Result<Result<(), Untrusted>, PortError> {
    let failed = |what: &str, e: windows::core::Error| PortError::Failed(format!("{what}: {e}"));

    let mut owner = PSID::default();
    let mut defaulted = windows::core::BOOL::default();
    // SAFETY: the descriptor is live, and the out pointers are valid for the call.
    unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) }
        .map_err(|e| failed("reading the owner", e))?;
    if owner.is_invalid() {
        return Ok(Err(Untrusted::Owner("nobody".into())));
    }
    let owner_sid = sid_text(owner)?;
    if ![SID_SYSTEM, SID_ADMINISTRATORS, SID_TRUSTED_INSTALLER].contains(&owner_sid.as_str()) {
        return Ok(Err(Untrusted::Owner(owner_sid)));
    }

    let mut present = windows::core::BOOL::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: as above.
    unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
        .map_err(|e| failed("reading permissions", e))?;
    if !present.as_bool() || dacl.is_null() {
        return Ok(Err(Untrusted::NoDacl));
    }

    // SAFETY: the DACL pointer stays valid as long as the descriptor does.
    let count = u32::from(unsafe { (*dacl).AceCount });
    for index in 0..count {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: the index is in range, and `ace` is a valid place for the result.
        unsafe { GetAce(dacl, index, &mut ace) }.map_err(|e| failed("reading permissions", e))?;
        // SAFETY: GetAce returned a pointer to an entry that starts with this header.
        let header = unsafe { *ace.cast::<ACE_HEADER>() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        // SAFETY: the type check above means this entry has the ACCESS_ALLOWED_ACE layout. Reading
        // through the raw pointer avoids creating a reference to the variable-length SID that follows.
        let mask = unsafe { std::ptr::addr_of!((*allowed).Mask).read_unaligned() };
        if mask & WRITE_RIGHTS == 0 {
            continue;
        }
        // SAFETY: the SID starts at SidStart, within the entry GetAce returned.
        let sid = PSID(
            unsafe { std::ptr::addr_of!((*allowed).SidStart) }
                .cast_mut()
                .cast(),
        );
        let text = sid_text(sid)?;
        if ![SID_SYSTEM, SID_ADMINISTRATORS, SID_TRUSTED_INSTALLER].contains(&text.as_str()) {
            return Ok(Err(Untrusted::WritableBy(text)));
        }
    }
    Ok(Ok(()))
}

/// Checks a directory can be trusted. Works without elevation.
pub fn check(dir: &Path) -> Result<Result<(), Untrusted>, PortError> {
    // Checked without following links, so a link can't hide behind a good target.
    let metadata = std::fs::symlink_metadata(dir)
        .map_err(|e| PortError::Failed(format!("reading {}: {e}", dir.display())))?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Ok(Err(Untrusted::Reparse));
    }

    let path = wide(&dir.to_string_lossy());
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the path outlives the call, and `descriptor` is a valid place for the result.
    let code = unsafe {
        GetNamedSecurityInfoW(
            &path,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            &mut descriptor,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(crate::util::win32_error("reading permissions", code));
    }
    let _guard = LocalMemory(descriptor.0);
    analyse(descriptor)
}

fn program_data() -> Result<PathBuf, PortError> {
    // SAFETY: the folder ID is a valid constant. The API allocates the returned string.
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }
        .map_err(|e| PortError::Failed(format!("finding ProgramData: {e}")))?;
    // SAFETY: the API succeeded, so this is a terminated string, which is copied before it's freed.
    let text = unsafe { path.to_string() };
    // SAFETY: this string came from SHGetKnownFolderPath, which says to free it with CoTaskMemFree.
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    let text = text.map_err(|e| PortError::Failed(format!("finding ProgramData: {e}")))?;
    Ok(PathBuf::from(text))
}

/// Where augeo keeps its state. It comes from the known-folder API, never an environment variable.
pub fn location() -> Result<PathBuf, PortError> {
    Ok(program_data()?.join("augeo"))
}

fn create_protected(dir: &Path) -> Result<(), PortError> {
    let sddl = wide(SDDL);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the string outlives the call, and `descriptor` is a valid place for the result.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &sddl,
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|e| PortError::Failed(format!("building permissions: {e}")))?;
    let _guard = LocalMemory(descriptor.0);

    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let path = wide(&dir.to_string_lossy());
    // SAFETY: the path and attributes outlive the call. Permissions apply atomically with creation,
    // so there's no moment when the directory exists with weaker ones.
    unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), Some(&attributes)) }
        .map_err(|e| PortError::Failed(format!("creating {}: {e}", dir.display())))
}

/// What opening the state directory found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// Ready, and safe to write to.
    Ready(PathBuf),
    /// A folder that couldn't be trusted was moved aside, and a new one created.
    Replaced {
        dir: PathBuf,
        reason: Untrusted,
        moved_to: PathBuf,
    },
}

/// Opens the state directory for an elevated writer, creating it if needed. A directory that can't be
/// trusted is moved aside rather than used. If it can't be moved, this fails and nothing gets written.
pub fn open_for_writing(dir: &Path) -> Result<Opened, PortError> {
    if !dir.exists() && std::fs::symlink_metadata(dir).is_err() {
        create_protected(dir)?;
        return confirm(dir).map(|()| Opened::Ready(dir.to_path_buf()));
    }
    match check(dir)? {
        Ok(()) => Ok(Opened::Ready(dir.to_path_buf())),
        Err(reason) => {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let parent = dir.parent().unwrap_or(dir);
            let moved_to = (1u32..10_000)
                .map(|n| parent.join(format!("{name}.untrusted-{n}")))
                .find(|p| std::fs::symlink_metadata(p).is_err())
                .ok_or_else(|| PortError::Failed("too many untrusted state folders".into()))?;
            std::fs::rename(dir, &moved_to).map_err(|e| {
                PortError::Denied(format!(
                    "{} can't be trusted because {reason}, and it couldn't be moved aside: {e}",
                    dir.display()
                ))
            })?;
            create_protected(dir)?;
            confirm(dir)?;
            Ok(Opened::Replaced {
                dir: dir.to_path_buf(),
                reason,
                moved_to,
            })
        }
    }
}

fn confirm(dir: &Path) -> Result<(), PortError> {
    match check(dir)? {
        Ok(()) => Ok(()),
        Err(reason) => Err(PortError::Failed(format!(
            "{} isn't safe even after creating it, because {reason}",
            dir.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the analysis on a descriptor built from an SDDL string, without touching the disk.
    fn analyse_sddl(sddl: &str) -> Result<(), Untrusted> {
        let text = wide(sddl);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: as in `create_protected`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &text,
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .unwrap();
        let _guard = LocalMemory(descriptor.0);
        analyse(descriptor).unwrap()
    }

    #[test]
    fn our_own_permissions_are_accepted() {
        assert_eq!(analyse_sddl(SDDL), Ok(()));
    }

    #[test]
    fn unsafe_permissions_are_rejected() {
        const USERS: &str = "S-1-5-32-545";
        const EVERYONE: &str = "S-1-1-0";
        const AUTH_USERS: &str = "S-1-5-11";
        for (sddl, sid) in [
            // Users can create files: the default rule on a new ProgramData folder.
            ("O:BAD:(A;OICI;FA;;;BA)(A;CI;0x6;;;BU)", USERS),
            ("O:BAD:(A;OICI;FA;;;BA)(A;OICI;FA;;;WD)", EVERYONE),
            ("O:BAD:(A;OICI;FA;;;BA)(A;OICI;GA;;;AU)", AUTH_USERS),
            ("O:BAD:(A;OICI;FA;;;BA)(A;OICI;GW;;;BU)", USERS),
            // Change permissions or take ownership is as good as write.
            ("O:BAD:(A;OICI;FA;;;BA)(A;OICI;WD;;;BU)", USERS),
            ("O:BAD:(A;OICI;FA;;;BA)(A;OICI;WO;;;BU)", USERS),
            // Delete would let a user swap the folder.
            ("O:BAD:(A;OICI;FA;;;BA)(A;;SD;;;BU)", USERS),
        ] {
            assert_eq!(
                analyse_sddl(sddl),
                Err(Untrusted::WritableBy(sid.into())),
                "{sddl}"
            );
        }
    }

    #[test]
    fn an_owner_other_than_admins_is_rejected() {
        let result = analyse_sddl("O:BUD:(A;OICI;FA;;;BA)");
        assert!(matches!(result, Err(Untrusted::Owner(_))), "{result:?}");
    }

    #[test]
    fn no_permissions_at_all_is_rejected() {
        assert_eq!(
            analyse_sddl("O:BAD:NO_ACCESS_CONTROL"),
            Err(Untrusted::NoDacl)
        );
    }

    #[test]
    fn read_only_access_for_users_and_inherit_only_entries_are_fine() {
        assert_eq!(
            analyse_sddl("O:BAD:(A;OICI;FA;;;BA)(A;OICI;FR;;;BU)"),
            Ok(())
        );
        assert_eq!(analyse_sddl("O:SYD:(A;OICI;FA;;;SY)"), Ok(()));
        // Applies only to children's permissions, not to this folder.
        assert_eq!(
            analyse_sddl("O:BAD:(A;OICI;FA;;;BA)(A;OICIIO;FA;;;CO)"),
            Ok(())
        );
    }

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("augeo-test-dir-{name}-{}", std::process::id()))
    }

    #[test]
    fn an_ordinary_user_folder_is_not_trusted() {
        let dir = scratch("ordinary");
        std::fs::create_dir_all(&dir).unwrap();
        let result = check(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn a_junction_is_not_trusted_even_if_its_target_is_fine() {
        let target = scratch("junction-target");
        let link = scratch("junction-link");
        std::fs::create_dir_all(&target).unwrap();
        let _ = std::fs::remove_dir(&link);
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
        let result = check(&link).unwrap();
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&target);
        assert_eq!(result, Err(Untrusted::Reparse));
    }

    #[test]
    fn the_location_is_under_program_data() {
        let location = location().unwrap();
        assert!(location.ends_with("augeo"), "{location:?}");
        assert!(location.parent().unwrap().exists());
    }

    #[cfg(feature = "admin-tests")]
    mod elevated {
        use super::*;

        fn base(name: &str) -> PathBuf {
            program_data()
                .unwrap()
                .join(format!("augeo-test-{name}-{}", std::process::id()))
        }

        #[test]
        fn creation_is_protected_from_the_first_moment() {
            let dir = base("create");
            assert_eq!(open_for_writing(&dir).unwrap(), Opened::Ready(dir.clone()));
            assert_eq!(check(&dir).unwrap(), Ok(()));
            // A second open finds it fine and leaves it alone.
            assert_eq!(open_for_writing(&dir).unwrap(), Opened::Ready(dir.clone()));
            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[test]
        fn a_squatted_folder_is_moved_aside() {
            let dir = base("squat");
            // Made the normal way, it inherits ProgramData's rule letting Users create files.
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("journal.log"), b"planted").unwrap();
            assert!(check(&dir).unwrap().is_err());

            let opened = open_for_writing(&dir).unwrap();
            let Opened::Replaced { moved_to, .. } = opened else {
                panic!("expected the folder to be replaced: {opened:?}");
            };
            assert_eq!(check(&dir).unwrap(), Ok(()));
            assert!(!dir.join("journal.log").exists());
            assert!(moved_to.join("journal.log").exists());
            std::fs::remove_dir_all(&dir).unwrap();
            std::fs::remove_dir_all(&moved_to).unwrap();
        }
    }
}
