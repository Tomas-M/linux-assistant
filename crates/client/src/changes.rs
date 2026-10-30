use std::{io::Read,path::{Path,PathBuf},time::Duration};
use serde_json::{json,Value};

pub struct Snapshot { path:PathBuf, before:Option<Option<Vec<u8>>> }

/// Unknown/unreadable files stay unknown; absence is distinct from an empty file.
fn contents(path:&Path)->Option<Option<Vec<u8>>> {
    let metadata=match std::fs::metadata(path) {
        Ok(metadata)=>metadata,
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Some(None),
        Err(_)=>return None,
    };
    if !metadata.is_file() || metadata.len()>1024*1024 { return None; }
    let mut bytes=Vec::new();
    std::fs::File::open(path).ok()?.take(1024*1024+1).read_to_end(&mut bytes).ok()?;
    (bytes.len()<=1024*1024).then_some(Some(bytes))
}

/// Inspect only the declared paths, with bounded reads and no recursive filesystem scan.
pub fn capture(cwd:&str,files:&[String])->Vec<Snapshot> {
    let mut snapshots=Vec::new();
    for file in files.iter().take(16) {
        let path=Path::new(cwd).join(file);
        if !snapshots.iter().any(|snapshot:&Snapshot|snapshot.path==path) { snapshots.push(Snapshot { before:contents(&path),path }); }
    }
    snapshots
}

/// A verified byte difference drives the edit indicator, even if a command failed or was stopped.
pub fn compare(snapshots:Vec<Snapshot>)->Vec<Value> {
    snapshots.into_iter().filter_map(|snapshot| {
        let before=snapshot.before?;
        let after=contents(&snapshot.path)?;
        if before==after { return None; }
        let kind=if before.is_none() { "Created" } else if after.is_none() { "Deleted" } else { "Edited" };
        let mut change=json!({"path":snapshot.path,"kind":kind,"added":null,"removed":null,"diff":"Binary or large file changed; text preview unavailable."});
        let (before,after)=(before.unwrap_or_default(),after.unwrap_or_default());
        if before.len()+after.len()<=256*1024 {
            if let (Ok(old),Ok(new))=(std::str::from_utf8(&before),std::str::from_utf8(&after)) {
                if !old.contains('\0') && !new.contains('\0') {
                    let diff=similar::TextDiff::configure().timeout(Duration::from_millis(100)).diff_lines(old,new);
                    let (mut added,mut removed)=(0,0);
                    for line in diff.iter_all_changes() {
                        match line.tag() { similar::ChangeTag::Insert=>added+=1,similar::ChangeTag::Delete=>removed+=1,_=>{} }
                    }
                    let mut preview=diff.unified_diff().context_radius(3).to_string();
                    if preview.len()>32*1024 {
                        let mut end=32*1024;
                        while !preview.is_char_boundary(end) { end-=1; }
                        preview.truncate(end); preview.push_str("\n[Diff preview truncated]");
                    }
                    change["added"]=json!(added); change["removed"]=json!(removed); change["diff"]=json!(preview);
                }
            }
        }
        Some(change)
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Read-only, missing, created, deleted, Unicode, and oversized files have distinct outcomes.
    #[test]
    fn reports_only_verified_changes()->anyhow::Result<()> {
        let directory=tempfile::tempdir()?;
        let cwd=directory.path().to_str().unwrap();
        std::fs::write(directory.path().join("世界.txt"),"one\ntwo\n")?;
        std::fs::write(directory.path().join("deleted"),"gone\n")?;
        let snapshots=capture(cwd,&["世界.txt".into(),"deleted".into(),"new".into(),"absent".into()]);
        std::fs::write(directory.path().join("世界.txt"),"one\nthree\n")?;
        std::fs::remove_file(directory.path().join("deleted"))?;
        std::fs::write(directory.path().join("new"),"new\n")?;
        let changes=compare(snapshots);
        assert_eq!(changes.len(),3);
        assert_eq!(changes[0]["added"],1); assert_eq!(changes[0]["removed"],1);
        assert_eq!(changes[1]["kind"],"Deleted"); assert_eq!(changes[2]["kind"],"Created");
        assert!(compare(capture(cwd,&["世界.txt".into()])).is_empty());
        std::fs::write(directory.path().join("large"),vec![b'x';1024*1024+1])?;
        let snapshots=capture(cwd,&["large".into()]);
        std::fs::write(directory.path().join("large"),"small")?;
        assert!(compare(snapshots).is_empty());
        Ok(())
    }
}
