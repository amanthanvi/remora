//! Handle-bound owner/DACL checks and atomic owner-only Windows custody.

use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    io::{Read, Write},
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
};

use anyhow::{anyhow, ensure};
use windows_sys::Win32::{
    Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
        GetSecurityDescriptorDacl, GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        MoveFileExW, READ_CONTROL, WRITE_DAC,
    },
    System::{
        SystemServices::ACCESS_ALLOWED_ACE_TYPE,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};
use zeroize::Zeroizing;

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

struct User(Vec<usize>);
impl User {
    fn current() -> anyhow::Result<Self> {
        let mut token = null_mut();
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut needed = 0;
        unsafe {
            GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut needed);
        }
        ensure!(
            (size_of::<TOKEN_USER>()..=4096).contains(&(needed as usize)),
            "invalid Windows token size"
        );
        // TOKEN_USER contains pointers; aligned storage must outlive its SID.
        let mut storage = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        check(unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                storage.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        })?;
        let user = Self(storage);
        ensure!(
            unsafe { IsValidSid(user.sid()) } != 0,
            "invalid Windows owner SID"
        );
        Ok(user)
    }

    fn sid(&self) -> PSID {
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    fn descriptor(&self) -> anyhow::Result<LocalAllocation> {
        let mut text = null_mut();
        check(unsafe { ConvertSidToStringSidW(self.sid(), &mut text) })?;
        let _allocation = LocalAllocation(text.cast());
        let mut len = 0;
        while len < 256 && unsafe { *text.add(len) } != 0 {
            len += 1;
        }
        ensure!(len < 256, "invalid Windows SID text");
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(text, len) })?;
        descriptor(&format!("O:{sid}D:P(A;;FA;;;{sid})"))
    }
}

fn descriptor(sddl: &str) -> anyhow::Result<LocalAllocation> {
    let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = null_mut();
    check(unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    })?;
    Ok(LocalAllocation(descriptor))
}

fn security(file: &File, user: &User, require_private: bool) -> anyhow::Result<LocalAllocation> {
    let mut owner = null_mut();
    let mut dacl = null_mut();
    let mut descriptor = null_mut();
    status(unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    })?;
    let descriptor = LocalAllocation(descriptor);
    ensure!(
        !owner.is_null() && unsafe { EqualSid(owner, user.sid()) } != 0,
        "Windows custody owner mismatch"
    );
    if require_private {
        validate_dacl(descriptor.0, dacl, user)?;
    }
    Ok(descriptor)
}

fn validate_dacl(
    descriptor: PSECURITY_DESCRIPTOR,
    dacl: *mut ACL,
    user: &User,
) -> anyhow::Result<()> {
    let mut control = 0;
    let mut revision = 0;
    check(unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) })?;
    ensure!(
        control & SE_DACL_PROTECTED != 0 && !dacl.is_null(),
        "Windows custody requires protected DACL"
    );
    ensure!(
        unsafe { (*dacl).AceCount } == 1,
        "Windows custody grants other principals"
    );
    let mut ace = null_mut();
    check(unsafe { GetAce(dacl, 0, &mut ace) })?;
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    ensure!(
        header.AceType as u32 == ACCESS_ALLOWED_ACE_TYPE
            && header.AceFlags == 0
            && header.AceSize as usize >= size_of::<ACCESS_ALLOWED_ACE>(),
        "invalid Windows custody ACE"
    );
    let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    let sid = (&allowed.SidStart as *const u32).cast_mut().cast();
    ensure!(
        unsafe { IsValidSid(sid) } != 0 && unsafe { EqualSid(sid, user.sid()) } != 0,
        "Windows custody grants another principal"
    );
    Ok(())
}

fn no_reparse(file: &File) -> anyhow::Result<()> {
    ensure!(
        file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "Windows custody reparse points are forbidden"
    );
    Ok(())
}

pub(super) fn read_private(path: &Path, limit: u64) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    let file = OpenOptions::new()
        .read(true)
        .access_mode(
            READ_CONTROL | FILE_READ_ATTRIBUTES | windows_sys::Win32::Foundation::GENERIC_READ,
        )
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    no_reparse(&file)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "invalid Windows custody file"
    );
    security(&file, &User::current()?, true)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Windows custody file too large"
    );
    Ok(bytes)
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let user = User::current()?;
    let descriptor = user.descriptor()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("invalid Windows custody path"))?;
    std::fs::create_dir_all(parent)?;
    // Keep the checked directory open without delete-sharing until replacement.
    let directory = OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(parent)?;
    no_reparse(&directory)?;
    ensure!(
        directory.metadata()?.is_dir(),
        "Windows custody parent is not a directory"
    );
    security(&directory, &user, false)?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = null_mut();
    check(unsafe {
        GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
    })?;
    ensure!(
        present != 0 && !dacl.is_null(),
        "missing Windows custody DACL"
    );
    status(unsafe {
        SetSecurityInfo(
            directory.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null(),
        )
    })?;
    security(&directory, &user, true)?;

    let pending = parent.join(format!(".relay-{}.pending", super::random_hex()));
    let result = (|| -> anyhow::Result<()> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let name = wide(&pending)?;
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE | READ_CONTROL,
                0,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        ensure!(
            handle != INVALID_HANDLE_VALUE,
            std::io::Error::last_os_error()
        );
        let mut file = unsafe { File::from_raw_handle(handle) };
        no_reparse(&file)?;
        security(&file, &user, true)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let target = wide(path)?;
        check(unsafe {
            MoveFileExW(
                name.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&pending);
    }
    result
}

fn wide(path: &Path) -> anyhow::Result<Vec<u16>> {
    let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
    ensure!(!value.contains(&0), "Windows custody path contains NUL");
    value.push(0);
    Ok(value)
}

fn check(value: i32) -> anyhow::Result<()> {
    if value == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn status(value: u32) -> anyhow::Result<()> {
    if value != 0 {
        return Err(std::io::Error::from_raw_os_error(value as i32).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_only_files_round_trip_and_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custody");
        write_private(&path, b"first").unwrap();
        assert_eq!(&*read_private(&path, 100).unwrap(), b"first");
        write_private(&path, b"second").unwrap();
        assert_eq!(&*read_private(&path, 100).unwrap(), b"second");
        assert!(read_private(&path, 2).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn broad_and_null_dacls_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custody");
        write_private(&path, b"secret").unwrap();
        let file = OpenOptions::new()
            .access_mode(READ_CONTROL | WRITE_DAC)
            .open(&path)
            .unwrap();
        let descriptor = descriptor("D:P(A;;FA;;;WD)").unwrap();
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = null_mut();
        check(unsafe {
            GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
        })
        .unwrap();
        status(unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            )
        })
        .unwrap();
        drop(file);
        assert!(read_private(&path, 100).is_err());
        let file = OpenOptions::new()
            .access_mode(READ_CONTROL | WRITE_DAC)
            .open(&path)
            .unwrap();
        status(unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null(),
                null(),
            )
        })
        .unwrap();
        drop(file);
        assert!(read_private(&path, 100).is_err());
    }
}
