use std::{io, mem::size_of, os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle}, ptr::{null,null_mut}};
use anyhow::{bail, Context, Result};
use tokio::process::Child;
use windows_sys::Win32::{Foundation::{INVALID_HANDLE_VALUE,ERROR_MORE_DATA}, System::{
    Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD},
    JobObjects::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, QueryInformationJobObject, TerminateJobObject, JobObjectBasicProcessIdList, JOBOBJECT_BASIC_PROCESS_ID_LIST, JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE},
    Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
}};

pub(super) struct ProcessGroup { job:OwnedHandle }

impl ProcessGroup {
    /// Assign the suspended shell to a job before it can spawn descendants.
    pub fn contain(child: &Child) -> Result<Self> {
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
                    return Ok(Self { job });
                }
                found = Thread32Next(snapshot.as_raw_handle(),&mut entry);
            }
            bail!("Suspended shell thread was not found")
        }
    }

    /// Normal launcher exit clears automatic termination while retaining the job for explicit Stop.
    pub fn preserve(&mut self)->Result<Vec<u32>> {
        let limits=JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if unsafe { SetInformationJobObject(self.job.as_raw_handle(),JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32) }==0 {
            return Err(io::Error::last_os_error()).context("Preserve background command processes");
        }
        self.pids()
    }

    /// Read current members from the owned job, growing the aligned buffer if more children appeared.
    pub fn pids(&mut self)->Result<Vec<u32>> {
        let mut capacity=16;
        loop {
            let mut buffer=vec![0usize;capacity+2];
            let result=unsafe { QueryInformationJobObject(self.job.as_raw_handle(),JobObjectBasicProcessIdList,
                buffer.as_mut_ptr().cast(),(buffer.len()*size_of::<usize>()) as u32,null_mut()) };
            let list=unsafe { &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
            if result!=0 {
                return Ok(unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(),list.NumberOfProcessIdsInList as usize) }.iter().map(|pid|*pid as u32).collect());
            }
            let error=io::Error::last_os_error();
            if error.raw_os_error()!=Some(ERROR_MORE_DATA as i32) { return Err(error).context("Read command job processes"); }
            capacity=(list.NumberOfAssignedProcesses as usize).max(capacity*2);
        }
    }

    /// Terminate this job's remaining members, including after its original shell has exited.
    pub fn stop(&mut self)->Result<()> {
        if unsafe { TerminateJobObject(self.job.as_raw_handle(),1) }==0 { return Err(io::Error::last_os_error()).context("Stop command job"); }
        Ok(())
    }
}
