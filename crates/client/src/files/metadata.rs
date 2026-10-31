use std::{fs::File,path::Path};
use anyhow::Result;
use tempfile::NamedTempFile;

/// Preserve ownership, mode, ACLs and other Linux attributes on the staged replacement.
pub(super) fn preserve(source:&File,temporary:&NamedTempFile)->Result<()> {
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        let metadata=source.metadata()?;
        std::os::unix::fs::chown(temporary.path(),Some(metadata.uid()),Some(metadata.gid()))?;
        temporary.as_file().set_permissions(metadata.permissions())?;
    }
    #[cfg(target_os="linux")] attributes(source,temporary.as_file())?;
    #[cfg(windows)] {
        use std::os::windows::{ffi::OsStrExt,fs::MetadataExt};
        use windows_sys::Win32::Storage::FileSystem::*;
        // Clear the staging file's temporary flag and retain the original user-visible attributes.
        let attributes=source.metadata()?.file_attributes() & (FILE_ATTRIBUTE_ARCHIVE|FILE_ATTRIBUTE_HIDDEN|FILE_ATTRIBUTE_NOT_CONTENT_INDEXED|FILE_ATTRIBUTE_OFFLINE|FILE_ATTRIBUTE_READONLY|FILE_ATTRIBUTE_SYSTEM|FILE_ATTRIBUTE_TEMPORARY);
        let path=temporary.path().as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        if unsafe { SetFileAttributesW(path.as_ptr(),if attributes==0 { FILE_ATTRIBUTE_NORMAL } else { attributes }) }==0 { return Err(std::io::Error::last_os_error().into()); }
    }
    #[cfg(not(any(unix,windows)))] let _=(source,temporary);
    Ok(())
}

/// Copy attributes exactly, removing inherited ACL entries absent from the original file.
#[cfg(target_os="linux")]
fn attributes(source:&File,target:&File)->Result<()> {
    use std::os::fd::AsRawFd;
    let original=read_attributes(source)?; let inherited=read_attributes(target)?;
    for name in inherited.keys().filter(|name|!original.contains_key(*name)) {
        if unsafe { libc::fremovexattr(target.as_raw_fd(),name.as_ptr().cast()) }<0 { return Err(std::io::Error::last_os_error().into()); }
    }
    for (name,value) in original {
        if inherited.get(&name)==Some(&value) { continue; }
        if unsafe { libc::fsetxattr(target.as_raw_fd(),name.as_ptr().cast(),value.as_ptr().cast(),value.len(),0) }<0 { return Err(std::io::Error::last_os_error().into()); }
    }
    Ok(())
}

/// Read names and binary values through the already-open inode, failing on concurrent metadata changes.
#[cfg(target_os="linux")]
fn read_attributes(file:&File)->Result<std::collections::HashMap<Vec<u8>,Vec<u8>>> {
    use std::os::fd::AsRawFd;
    use std::io::Error;
    let mut attributes=std::collections::HashMap::new();
    unsafe {
        let size=libc::flistxattr(file.as_raw_fd(),std::ptr::null_mut(),0);
        if size<0 {
            let error=Error::last_os_error();
            if error.raw_os_error()==Some(libc::ENOTSUP) { return Ok(attributes); }
            return Err(error.into());
        }
        let mut names=vec![0u8;size as usize];
        let size=libc::flistxattr(file.as_raw_fd(),names.as_mut_ptr().cast(),names.len());
        if size<0 { return Err(Error::last_os_error().into()); }
        for name in names[..size as usize].split_inclusive(|byte|*byte==0) {
            let size=libc::fgetxattr(file.as_raw_fd(),name.as_ptr().cast(),std::ptr::null_mut(),0);
            if size<0 { return Err(Error::last_os_error().into()); }
            let mut value=vec![0u8;size as usize];
            let size=libc::fgetxattr(file.as_raw_fd(),name.as_ptr().cast(),value.as_mut_ptr().cast(),value.len());
            if size<0 { return Err(Error::last_os_error().into()); }
            value.truncate(size as usize); attributes.insert(name.to_vec(),value);
        }
    }
    Ok(attributes)
}

/// Commit with the platform's metadata-preserving replacement operation.
pub(super) fn replace(temporary:NamedTempFile,target:&Path)->Result<()> {
    #[cfg(not(windows))] { temporary.persist(target)?; }
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
        // Keep a backup even on failure: Windows may move the original before returning an error.
        let backup=NamedTempFile::new_in(target.parent().unwrap())?.into_temp_path();
        std::fs::remove_file(&backup)?;
        let replacement=temporary.into_temp_path();
        let original=target.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let staged=replacement.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let saved=backup.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        if unsafe { ReplaceFileW(original.as_ptr(),staged.as_ptr(),saved.as_ptr(),0,std::ptr::null(),std::ptr::null()) }==0 {
            let error=std::io::Error::last_os_error();
            if backup.exists() {
                let backup=backup.keep()?;
                anyhow::bail!("Could not replace file: {error}. Inspect the current file before retrying; the original backup is at {}",backup.display());
            }
            anyhow::bail!("Could not replace file: {error}. Inspect the current file before retrying");
        }
    }
    Ok(())
}
