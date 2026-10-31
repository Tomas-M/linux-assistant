use super::*;
use std::{path::PathBuf,sync::atomic::{AtomicBool,Ordering}};

/// Handle filesystem mutations explicitly, without a shell or implicit overwrites.
pub(super) fn execute(action:&LocalAction,stopped:&AtomicBool)->Result<String> {
    match action {
        LocalAction::GetMetadata { path,follow_symlinks,.. }=> {
            let path=Path::new(path); ensure!(path.is_absolute(),"File paths must be absolute");
            let metadata=if *follow_symlinks { fs::metadata(path)? } else { fs::symlink_metadata(path)? };
            let time=|value:std::io::Result<std::time::SystemTime>|value.ok().and_then(|time|time.duration_since(std::time::UNIX_EPOCH).ok()).map(|time|time.as_secs());
            let mut value=json!({"path":path,"type":if metadata.is_symlink() { "symlink" } else if metadata.is_dir() { "directory" } else if metadata.is_file() { "file" } else { "special" },"bytes":metadata.len(),"readonly":metadata.permissions().readonly(),"modified":time(metadata.modified()),"created":time(metadata.created()),"symlink_target":fs::read_link(path).ok()});
            value["accessed"]=json!(time(metadata.accessed()));
            #[cfg(unix)] { use std::os::unix::fs::MetadataExt; value["mode"]=json!(format!("{:04o}",metadata.mode() & 0o7777)); value["uid"]=json!(metadata.uid()); value["gid"]=json!(metadata.gid()); value["links"]=json!(metadata.nlink()); }
            json_output(value)
        }
        LocalAction::CreateDirectory { path,recursive,.. }=> {
            ensure!(Path::new(path).is_absolute(),"Directory path must be absolute");
            if *recursive { fs::create_dir_all(path)?; } else { fs::create_dir(path)?; }
            Ok(format!("Created directory {path}"))
        }
        LocalAction::CopyPath { source,destination,recursive,.. }=> {
            let source=Path::new(source); let destination=destination_path(Path::new(destination))?;
            ensure!(source.is_absolute(),"Source must be absolute");
            let metadata=fs::symlink_metadata(source)?;
            if metadata.is_dir() && !metadata.is_symlink() {
                ensure!(*recursive,"Copying a directory requires recursive=true");
                ensure!(!destination.starts_with(fs::canonicalize(source)?),"Cannot copy a directory into itself");
                let staging=tempfile::tempdir_in(destination.parent().unwrap())?;
                copy_tree(source,staging.path(),stopped)?;
                ensure!(!stopped.load(Ordering::SeqCst),"Copy stopped before commit");
                move_new(staging.path(),&destination)?;
            } else { copy_entry(source,&destination,stopped)?; }
            Ok(format!("Copied {} to {}",source.display(),destination.display()))
        }
        LocalAction::MovePath { source,destination,.. }=> {
            let source=destination_path(Path::new(source))?; let destination=destination_path(Path::new(destination))?;
            ensure!(!destination.starts_with(&source),"Cannot move a path into itself");
            ensure!(!stopped.load(Ordering::SeqCst),"Move stopped before commit");
            move_new(&source,&destination)?;
            Ok(format!("Moved {} to {}",source.display(),destination.display()))
        }
        LocalAction::RemovePath { path,recursive,.. }=> {
            let path=destination_path(Path::new(path))?;
            let metadata=fs::symlink_metadata(&path)?;
            if metadata.is_dir() && !metadata.is_symlink() { ensure!(fs::canonicalize(&path)?.parent().is_some(),"Refusing to remove a filesystem root"); }
            ensure!(!stopped.load(Ordering::SeqCst),"Deletion stopped before commit");
            if metadata.is_dir() && !metadata.is_symlink() {
                // The standard library uses platform protections against symlink substitution races.
                if *recursive { fs::remove_dir_all(&path)?; } else { fs::remove_dir(&path)?; }
            } else {
                #[cfg(windows)] {
                    use std::os::windows::fs::FileTypeExt;
                    if metadata.file_type().is_symlink_dir() { fs::remove_dir(&path)?; } else { fs::remove_file(&path)?; }
                }
                #[cfg(not(windows))] fs::remove_file(&path)?;
            }
            Ok(format!("Deleted {}",path.display()))
        }
        LocalAction::CreateSymlink { path,target,directory,.. }=> {
            let path=destination_path(Path::new(path))?;
            ensure!(!target.is_empty() && target.len()<=4096 && !target.contains('\0'),"Invalid symbolic-link target");
            symlink(Path::new(target),&path,*directory)?;
            Ok(format!("Created symbolic link {} pointing to {target}",path.display()))
        }
        LocalAction::SetPermissions { path,mode,.. }=> {
            ensure!(Path::new(path).is_absolute(),"File paths must be absolute");
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                ensure!(!mode.is_empty() && mode.len()<=4 && mode.bytes().all(|byte|(b'0'..=b'7').contains(&byte)),"Use an octal permission mode, such as 644 or 755");
                fs::set_permissions(path,fs::Permissions::from_mode(u32::from_str_radix(mode,8)?))?;
            }
            #[cfg(windows)] {
                ensure!(mode=="readonly" || mode=="writable","On Windows use readonly or writable; use an approved command for ACL changes");
                let mut permissions=fs::metadata(path)?.permissions(); permissions.set_readonly(mode=="readonly"); fs::set_permissions(path,permissions)?;
            }
            Ok(format!("Set permissions of {path} to {mode}"))
        }
        _=>anyhow::bail!("Not a filesystem management operation"),
    }
}

/// Canonicalize the parent only: symlink operations affect the link, never its target.
fn destination_path(path:&Path)->Result<PathBuf> {
    ensure!(path.is_absolute(),"File paths must be absolute");
    let name=path.file_name().context("A filesystem root cannot be used as a file destination")?;
    Ok(fs::canonicalize(path.parent().context("Missing parent directory")?)?.join(name))
}

/// Recursively copy directory contents into private staging, never traversing symbolic links.
fn copy_tree(source:&Path,target:&Path,stopped:&AtomicBool)->Result<()> {
    let mut pending=vec![(source.to_owned(),target.to_owned(),false)];
    while let Some((source,target,visited))=pending.pop() {
        ensure!(!stopped.load(Ordering::SeqCst),"Copy stopped before commit");
        if visited { fs::set_permissions(target,fs::metadata(source)?.permissions())?; continue; }
        pending.push((source.clone(),target.clone(),true));
        for entry in fs::read_dir(source)? {
            let entry=entry?; let destination=target.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                fs::create_dir(&destination)?; pending.push((entry.path(),destination,false));
            } else { copy_entry(&entry.path(),&destination,stopped)?; }
        }
    }
    Ok(())
}

/// Copy regular files through a staged no-clobber commit; preserve link targets verbatim.
fn copy_entry(source:&Path,target:&Path,stopped:&AtomicBool)->Result<()> {
    ensure!(!stopped.load(Ordering::SeqCst),"Copy stopped before commit");
    let metadata=fs::symlink_metadata(source)?;
    if metadata.is_symlink() {
        #[cfg(windows)] let directory={ use std::os::windows::fs::FileTypeExt; metadata.file_type().is_symlink_dir() };
        #[cfg(not(windows))] let directory=false;
        return symlink(&fs::read_link(source)?,target,directory);
    }
    ensure!(metadata.is_file(),"Only regular files, symbolic links and directories can be copied");
    let mut source=open_file(source)?; let mut temporary=tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
    let mut buffer=[0u8;64*1024];
    loop {
        ensure!(!stopped.load(Ordering::SeqCst),"Copy stopped before commit");
        let count=source.read(&mut buffer)?; if count==0 { break; }
        temporary.write_all(&buffer[..count])?;
    }
    #[cfg(unix)] temporary.as_file().set_permissions(metadata.permissions())?;
    temporary.as_file().sync_all()?;
    ensure!(!stopped.load(Ordering::SeqCst),"Copy stopped before commit");
    temporary.persist_noclobber(target)?;
    #[cfg(windows)] fs::set_permissions(target,metadata.permissions())?;
    Ok(())
}

/// Rename atomically without replacing an existing file or directory, including a racing creator.
fn move_new(source:&Path,destination:&Path)->Result<()> {
    #[cfg(target_os="linux")] {
        use std::os::unix::ffi::OsStrExt;
        let source=std::ffi::CString::new(source.as_os_str().as_bytes())?; let destination=std::ffi::CString::new(destination.as_os_str().as_bytes())?;
        if unsafe { libc::renameat2(libc::AT_FDCWD,source.as_ptr(),libc::AT_FDCWD,destination.as_ptr(),libc::RENAME_NOREPLACE) }<0 { return Err(std::io::Error::last_os_error().into()); }
    }
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        let source=source.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>(); let destination=destination.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        if unsafe { windows_sys::Win32::Storage::FileSystem::MoveFileExW(source.as_ptr(),destination.as_ptr(),0) }==0 { return Err(std::io::Error::last_os_error().into()); }
    }
    Ok(())
}

/// Preserve relative target semantics and expose Windows privilege failures without elevating silently.
fn symlink(target:&Path,path:&Path,directory:bool)->Result<()> {
    #[cfg(unix)] { let _=directory; std::os::unix::fs::symlink(target,path)?; }
    #[cfg(windows)] { if directory { std::os::windows::fs::symlink_dir(target,path)?; } else { std::os::windows::fs::symlink_file(target,path)?; } }
    Ok(())
}
