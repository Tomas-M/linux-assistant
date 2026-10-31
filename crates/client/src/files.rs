use std::{fs::{self,File,OpenOptions},io::{Read,Seek,SeekFrom,Write,BufRead,BufReader},path::Path};
use anyhow::{ensure,Context,Result};
use protocol::LocalAction;
use serde_json::json;

/// Execute dedicated file tools with bounded reads and exact-match edits, without shell quoting or helper binaries.
pub fn execute(action:&LocalAction,stopped:&std::sync::atomic::AtomicBool)->Result<String> {
    ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Task stopped");
    let path=match action {
        LocalAction::ReadFile { path,.. }|LocalAction::ListDirectory { path,.. }|LocalAction::SearchFiles { path,.. }|LocalAction::ApplyPatch { path,.. }|LocalAction::WriteFile { path,.. }=>Path::new(path),
        _=>anyhow::bail!("Not a file operation"),
    };
    ensure!(path.is_absolute(),"File paths must be absolute");
    match action {
        LocalAction::ReadFile { offset,limit,.. }=> {
            let mut file=File::open(path)?; let total=file.metadata()?.len();
            file.seek(SeekFrom::Start(*offset))?;
            let mut bytes=Vec::new(); file.take((*limit).clamp(4,24_000)).read_to_end(&mut bytes)?;
            let start=bytes.iter().position(|byte|byte & 0xc0!=0x80).unwrap_or(bytes.len());
            let end=match std::str::from_utf8(&bytes[start..]) { Err(error) if error.error_len().is_none() && offset+(bytes.len() as u64)<total=>start+error.valid_up_to(),_=>bytes.len() };
            let text=std::str::from_utf8(&bytes[start..end]).context("File is not UTF-8 text")?;
            ensure!(!text.contains('\0'),"File contains binary data");
            Ok(json!({"path":path,"offset":offset+start as u64,"next_offset":offset+end as u64,"total_bytes":total,"content":text}).to_string())
        }
        LocalAction::ListDirectory { offset,limit,.. }=> {
            let mut entries=fs::read_dir(path)?.take(100_001).collect::<std::io::Result<Vec<_>>>()?;
            let truncated=entries.len()>100_000; entries.truncate(100_000); entries.sort_by_key(|entry|entry.file_name());
            let count=entries.len(); let limit=(*limit).clamp(1,200);
            let files=entries.into_iter().skip(*offset).take(limit).map(|entry| {
                let kind=entry.file_type()?;
                Ok(json!({"name":entry.file_name().to_string_lossy(),"type":if kind.is_symlink() { "symlink" } else if kind.is_dir() { "directory" } else { "file" }}))
            }).collect::<Result<Vec<_>>>()?;
            Ok(json!({"path":path,"entries":files,"next_offset":offset.saturating_add(files.len()),"total_entries":count,"truncated":truncated}).to_string())
        }
        LocalAction::SearchFiles { pattern,case_sensitive,recursive,.. }=> {
            ensure!(!pattern.is_empty() && pattern.len()<=4000,"Search text must contain 1 to 4000 bytes");
            let pattern=if *case_sensitive { pattern.clone() } else { pattern.to_lowercase() };
            let mut pending=vec![path.to_path_buf()]; let mut matches=Vec::new(); let mut skipped=0; let mut visited=0;
            while let Some(path)=pending.pop() {
                ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Search stopped");
                if visited>=10_000 || matches.len()>=100 { pending.push(path); break; }
                visited+=1;
                let metadata=match fs::symlink_metadata(&path) { Ok(metadata)=>metadata,Err(_)=> { skipped+=1; continue; } };
                if metadata.is_symlink() { skipped+=1; continue; }
                if metadata.is_dir() {
                    let entries=match fs::read_dir(&path) { Ok(entries)=>entries,Err(_)=> { skipped+=1; continue; } };
                    for entry in entries.take(10_001).flatten() {
                        if pending.len()>=10_000 { skipped+=1; break; }
                        if *recursive || entry.file_type().is_ok_and(|kind|kind.is_file()) { pending.push(entry.path()); }
                    }
                    continue;
                }
                if !metadata.is_file() || metadata.len()>16*1024*1024 { skipped+=1; continue; }
                let file=match File::open(&path) { Ok(file)=>file,Err(_)=> { skipped+=1; continue; } };
                let mut reader=BufReader::new(file); let mut line=Vec::new(); let mut number=0;
                loop {
                    ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Search stopped");
                    line.clear(); if reader.by_ref().take(64*1024+1).read_until(b'\n',&mut line)?==0 { break; }
                    number+=1;
                    if line.len()>64*1024 { skipped+=1; break; }
                    let text=match std::str::from_utf8(&line) { Ok(text) if !text.contains('\0')=>text, _=> { skipped+=1; break; } };
                    let found=if *case_sensitive { text.contains(&pattern) } else { text.to_lowercase().contains(&pattern) };
                    if found {
                        matches.push(json!({"path":path,"line":number,"text":text.chars().take(1000).collect::<String>()}));
                        if matches.len()>=100 { break; }
                    }
                }
            }
            Ok(json!({"matches":matches,"visited":visited,"skipped":skipped,"truncated":!pending.is_empty() || matches.len()>=100}).to_string())
        }
        LocalAction::ApplyPatch { old_text,new_text,.. }=> {
            ensure!(!old_text.is_empty(),"Replacement text must identify a nonempty exact block");
            let target=fs::canonicalize(path)?; let metadata=fs::metadata(&target)?;
            ensure!(metadata.is_file() && metadata.len()<=16*1024*1024,"Editing requires a text file up to 16 MiB");
            OpenOptions::new().write(true).open(&target)?;
            let original=fs::read(&target)?;
            let text=std::str::from_utf8(&original).context("File is not UTF-8 text")?;
            ensure!(text.matches(old_text).count()==1,"The old text must match exactly once; read the current file and narrow the replacement");
            let replacement=text.replacen(old_text,new_text,1);
            let mut temporary=tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
            #[cfg(unix)] {
                use std::os::unix::fs::MetadataExt;
                std::os::unix::fs::chown(temporary.path(),Some(metadata.uid()),Some(metadata.gid()))?;
            }
            temporary.as_file().set_permissions(metadata.permissions())?;
            temporary.write_all(replacement.as_bytes())?; temporary.as_file().sync_all()?;
            ensure!(fs::read(&target)?==original,"File changed while preparing the edit; read it again");
            ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Edit stopped before commit");
            temporary.persist(&target)?;
            Ok(format!("Edited {}",path.display()))
        }
        LocalAction::WriteFile { content,.. }=> {
            let mut file=OpenOptions::new().write(true).create_new(true).open(path)?;
            file.write_all(content.as_bytes())?; file.sync_all()?;
            Ok(format!("Created {}",path.display()))
        }
        _=>unreachable!(),
    }
}
