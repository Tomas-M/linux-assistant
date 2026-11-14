use std::os::windows::ffi::OsStrExt;
use anyhow::Result;
use windows_sys::Win32::{Foundation::CloseHandle, System::{Threading::{CreateProcessW,PROCESS_INFORMATION,STARTUPINFOW,DETACHED_PROCESS,CREATE_NEW_PROCESS_GROUP}}};

/// Start a detached daemon without inheriting PowerShell's capture pipes or any other launcher handles.
pub fn spawn(arguments:&[std::ffi::OsString]) -> Result<()> {
    let executable=std::env::current_exe()?;
    let executable:Vec<u16>=executable.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut command=vec![b'"' as u16];
    command.extend_from_slice(&executable[..executable.len()-1]);
    command.push(b'"' as u16);
    for argument in arguments {
        command.extend([b' ' as u16,b'"' as u16]);
        let mut slashes=0;
        for ch in argument.encode_wide() {
            if ch==b'\\' as u16 { slashes+=1; continue; }
            command.extend(std::iter::repeat_n(b'\\' as u16,if ch==b'"' as u16 { slashes*2+1 } else { slashes }));
            command.push(ch); slashes=0;
        }
        command.extend(std::iter::repeat_n(b'\\' as u16,slashes*2)); command.push(b'"' as u16);
    }
    command.push(0);
    let startup=STARTUPINFOW { cb:std::mem::size_of::<STARTUPINFOW>() as u32,..unsafe { std::mem::zeroed() } };
    let mut process:PROCESS_INFORMATION=unsafe { std::mem::zeroed() };
    // No handles are inherited. The daemon keeps no on-disk log or session files.
    let created=unsafe { CreateProcessW(executable.as_ptr(),command.as_mut_ptr(),std::ptr::null(),std::ptr::null(),0,
        DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP,std::ptr::null(),std::ptr::null(),&startup,&mut process) };
    if created==0 { return Err(std::io::Error::last_os_error().into()); }
    unsafe { CloseHandle(process.hThread); CloseHandle(process.hProcess); }
    Ok(())
}

