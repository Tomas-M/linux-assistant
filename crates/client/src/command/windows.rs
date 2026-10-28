use std::{io, mem::size_of, os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle}, ptr::null};
use anyhow::{bail, Context, Result};
use tokio::process::Child;
use windows_sys::Win32::{Foundation::INVALID_HANDLE_VALUE, System::{
    Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD},
    JobObjects::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE},
    Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
}};

/// Assign the suspended shell to a job before it can spawn descendants; closing the job kills them.
pub fn contain(child: &Child) -> Result<OwnedHandle> {
    // All handles are checked before ownership transfer and closed on every error path.
    unsafe {
        let handle = CreateJobObjectW(null(),null());
        if handle.is_null() { return Err(io::Error::last_os_error()).context("Create command job"); }
        let job = OwnedHandle::from_raw_handle(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(job.as_raw_handle(),JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32)==0 {
            return Err(io::Error::last_os_error()).context("Configure command job");
        }
        if AssignProcessToJobObject(job.as_raw_handle(),child.raw_handle().unwrap())==0 {
            return Err(io::Error::last_os_error()).context("Assign shell to command job");
        }
        // Tokio retains the process handle but not its primary thread handle. Find it while suspended.
        let handle = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD,0);
        if handle==INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()).context("Find suspended shell thread"); }
        let snapshot = OwnedHandle::from_raw_handle(handle);
        let mut entry = THREADENTRY32 { dwSize:size_of::<THREADENTRY32>() as u32,..Default::default() };
        let mut found = Thread32First(snapshot.as_raw_handle(),&mut entry);
        while found!=0 {
            if entry.th32OwnerProcessID==child.id().unwrap() {
                let handle = OpenThread(THREAD_SUSPEND_RESUME,0,entry.th32ThreadID);
                if handle.is_null() { return Err(io::Error::last_os_error()).context("Open suspended shell thread"); }
                let thread = OwnedHandle::from_raw_handle(handle);
                if ResumeThread(thread.as_raw_handle())==u32::MAX { return Err(io::Error::last_os_error()).context("Resume shell"); }
                return Ok(job);
            }
            found = Thread32Next(snapshot.as_raw_handle(),&mut entry);
        }
        bail!("Suspended shell thread was not found")
    }
}
