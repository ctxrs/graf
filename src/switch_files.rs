//! Permissions and publication of migration files containing configuration secrets.
use std::path::Path;

use anyhow::Result;
use tempfile::NamedTempFile;

/// Restrict a newly created, empty file (or directory) before writing secrets.
/// Callers must own the path and must not use this to repair an exposed backup.
pub fn protect(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.metadata()?.is_dir() {
            0o700
        } else {
            0o600
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(windows)]
    windows::protect(path)?;
    #[cfg(not(any(unix, windows)))]
    anyhow::bail!("private migration files are unsupported on this platform");
    Ok(())
}

/// Publish a protected temporary file. Existing Windows configs retain their DACL.
pub fn replace(temp: NamedTempFile, path: &Path, exists: bool) -> Result<()> {
    if !exists {
        temp.persist_noclobber(path)?;
    } else {
        #[cfg(windows)]
        windows::replace(temp, path)?;
        #[cfg(not(windows))]
        temp.persist(path)?;
    }
    Ok(())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use anyhow::{Context, ensure};
    use std::{
        io,
        os::windows::{ffi::OsStrExt, io::FromRawHandle, io::OwnedHandle},
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Security::{
            ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx,
            Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW},
            CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, GetTokenInformation, InitializeAcl,
            OBJECT_INHERIT_ACE, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_MAX_SID_SIZE,
            TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{FILE_ALL_ACCESS, ReplaceFileW},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    fn wide(path: &Path) -> Result<Vec<u16>> {
        let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
        ensure!(!wide.contains(&0), "file path contains a NUL");
        wide.push(0);
        Ok(wide)
    }

    pub(super) fn protect(path: &Path) -> Result<()> {
        let inheritance = if path.metadata()?.is_dir() {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            0
        };
        let path = wide(path)?;
        // All buffers are aligned for their native structures and remain alive
        // until SetNamedSecurityInfoW copies the ACL. OwnedHandle closes the token.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error()).context("cannot open current user token");
            }
            let _token = OwnedHandle::from_raw_handle(token);
            let mut user = [0usize;
                (size_of::<TOKEN_USER>() + SECURITY_MAX_SID_SIZE as usize)
                    .div_ceil(size_of::<usize>())];
            let mut returned = 0;
            if GetTokenInformation(
                token,
                TokenUser,
                user.as_mut_ptr().cast(),
                size_of_val(&user) as u32,
                &mut returned,
            ) == 0
            {
                return Err(io::Error::last_os_error()).context("cannot read current user SID");
            }
            let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let mut acl = [0u32;
                (size_of::<ACL>()
                    + size_of::<ACCESS_ALLOWED_ACE>()
                    + SECURITY_MAX_SID_SIZE as usize)
                    .div_ceil(size_of::<u32>())];
            let acl_size = size_of_val(&acl) as u32;
            let acl = acl.as_mut_ptr().cast::<ACL>();
            if InitializeAcl(acl, acl_size, ACL_REVISION) == 0
                || AddAccessAllowedAceEx(acl, ACL_REVISION, inheritance, FILE_ALL_ACCESS, sid) == 0
            {
                return Err(io::Error::last_os_error()).context("cannot build private file ACL");
            }
            // Replace the entire DACL and disable inheritance: shared project
            // readers must never gain access to the configuration backup.
            let error = SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null(),
            );
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error as i32))
                    .context("cannot protect migration file");
            }
        }
        Ok(())
    }

    pub(super) fn replace(temp: NamedTempFile, path: &Path) -> Result<()> {
        let destination = wide(path)?;
        // Close the file handle before replacement; keep TempPath for cleanup.
        let temp = temp.into_temp_path();
        let source = wide(&temp)?;
        // ReplaceFileW preserves the original DACL. Flags must remain zero:
        // IGNORE_ACL_ERRORS / IGNORE_MERGE_ERRORS can silently lose protection.
        // https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-replacefilew
        if unsafe {
            ReplaceFileW(
                destination.as_ptr(),
                source.as_ptr(),
                null(),
                0,
                null(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error()).context("cannot replace configuration file");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};

    #[test]
    fn publishes_new_file_without_clobbering_existing_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("record.json");
        let mut temp = NamedTempFile::new_in(directory.path())?;
        protect(temp.path())?;
        temp.write_all(b"secret")?;
        replace(temp, &path, false)?;
        let other = NamedTempFile::new_in(directory.path())?;
        protect(other.path())?;
        assert!(replace(other, &path, false).is_err());
        assert_eq!(fs::read(&path)?, b"secret");
        Ok(())
    }

    #[test]
    fn replaces_existing_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("config.json");
        fs::write(&path, b"before")?;
        protect(&path)?;
        let mut temp = NamedTempFile::new_in(directory.path())?;
        protect(temp.path())?;
        temp.write_all(b"after")?;
        replace(temp, &path, true)?;
        assert_eq!(fs::read(&path)?, b"after");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn removes_group_and_other_permissions() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let file = NamedTempFile::new_in(directory.path())?;
        for (path, mode) in [(file.path(), 0o600), (directory.path(), 0o700)] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o777))?;
            protect(path)?;
            assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, mode);
        }
        Ok(())
    }
}
