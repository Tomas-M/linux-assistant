use anyhow::Result;
use tokio::process::Child;

pub(super) struct ProcessGroup { id:i32,terminate_on_drop:bool,members:Vec<(u32,u64)> }

impl ProcessGroup {
    /// The shell starts its own process group before executing any approved command.
    pub fn contain(child:&Child)->Result<Self> { Ok(Self { id:child.id().unwrap() as i32,terminate_on_drop:true,members:vec![] }) }

    /// Snapshot surviving members and leave them running when their launcher has exited.
    pub fn preserve(&mut self)->Result<Vec<u32>> {
        self.terminate_on_drop=false;
        self.members=self.snapshot()?;
        Ok(self.members.iter().map(|member|member.0).collect())
    }

    /// Retain ownership only while a previously observed process identity still belongs to this group.
    pub fn pids(&mut self)->Result<Vec<u32>> {
        let mut members=self.snapshot()?;
        if !self.members.iter().any(|member|members.contains(member)) { members.clear(); }
        self.members=members;
        Ok(self.members.iter().map(|member|member.0).collect())
    }

    /// Match group IDs and start times; names may contain spaces or international characters.
    fn snapshot(&self)->Result<Vec<(u32,u64)>> {
        let mut members=Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let entry=entry?;
            let Ok(pid)=entry.file_name().to_string_lossy().parse::<u32>() else { continue; };
            let Ok(stat)=std::fs::read_to_string(entry.path().join("stat")) else { continue; };
            let Some((_,fields))=stat.rsplit_once(')') else { continue; };
            let fields=fields.split_whitespace().collect::<Vec<_>>();
            if fields.len()<20 || matches!(fields[0],"Z"|"X") { continue; }
            if fields[2].parse::<i32>()?==self.id { members.push((pid,fields[19].parse()?)); }
        }
        members.sort_unstable();
        Ok(members)
    }

    /// Allow cleanup on explicit Stop, then forcibly terminate surviving members after five seconds.
    pub async fn stop(&mut self)->Result<()> {
        if !self.terminate_on_drop && self.pids()?.is_empty() { return Ok(()); }
        self.members=self.snapshot()?;
        if self.members.is_empty() { self.terminate_on_drop=false; return Ok(()); }
        self.terminate_on_drop=true;
        self.signal(libc::SIGTERM)?;
        let deadline=tokio::time::Instant::now()+std::time::Duration::from_secs(5);
        while !self.pids()?.is_empty() {
            if tokio::time::Instant::now()>=deadline { self.signal(libc::SIGKILL)?; break; }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        self.terminate_on_drop=false;
        Ok(())
    }

    /// Ignore a group which exited between the ownership check and signal delivery.
    fn signal(&self,signal:i32)->Result<()> {
        if unsafe { libc::kill(-self.id,signal) }==-1 {
            let error=std::io::Error::last_os_error();
            if error.raw_os_error()!=Some(libc::ESRCH) { return Err(error.into()); }
        }
        Ok(())
    }
}

impl Drop for ProcessGroup {
    /// Cancelled foreground execution still cleans up its tree; completed launchers do not.
    fn drop(&mut self) { if self.terminate_on_drop { let _=self.signal(libc::SIGKILL); } }
}
