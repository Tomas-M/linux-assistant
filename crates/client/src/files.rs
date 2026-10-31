use std::{fs::{self,File,OpenOptions},io::{Read,Seek,SeekFrom,Write,BufRead,BufReader},path::Path};
use anyhow::{ensure,Context,Result};
use protocol::LocalAction;
use serde_json::json;
mod metadata;

/// Execute dedicated file tools with bounded reads and exact-match edits, without shell quoting or helper binaries.
pub fn execute(action:&LocalAction,stopped:&std::sync::atomic::AtomicBool)->Result<String> {
    ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Task stopped");
    let path=match action {
        LocalAction::ReadFile { path,.. }|LocalAction::ListDirectory { path,.. }|LocalAction::SearchFiles { path,.. }|LocalAction::ApplyPatch { path,.. }|LocalAction::WriteFile { path,.. }=>Path::new(path),
        _=>anyhow::bail!("Not a file operation"),
    };
    ensure!(path.is_absolute(),"File paths must be absolute");
    match action {
        LocalAction::ReadFile { offset,limit,.. }=>read_file(path,*offset,*limit),
        LocalAction::ListDirectory { offset,limit,.. }=> {
            let mut entries=fs::read_dir(path)?.take(100_001).collect::<std::io::Result<Vec<_>>>()?;
            let capped=entries.len()>100_000; entries.truncate(100_000); entries.sort_by_key(|entry|entry.file_name());
            let count=entries.len(); let limit=(*limit).clamp(1,200);
            let mut result=json!({"path":path,"entries":[],"next_offset":offset,"total_entries":if capped { None } else { Some(count) },"truncated":capped,"has_more":false});
            let mut used=result.to_string().len()+64;
            let mut files=Vec::new();
            for entry in entries.into_iter().skip(*offset).take(limit) {
                ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Directory listing stopped");
                let kind=entry.file_type()?;
                let item=json!({"name":entry.file_name().to_string_lossy(),"type":if kind.is_symlink() { "symlink" } else if kind.is_dir() { "directory" } else { "file" }});
                let bytes=item.to_string().len()+1;
                if used+bytes>protocol::TOOL_OUTPUT_BYTES { break; }
                used+=bytes; files.push(item);
            }
            let next=offset.saturating_add(files.len());
            ensure!(!files.is_empty() || *offset>=count,"Directory metadata exceeds the output budget");
            result["entries"]=json!(files); result["next_offset"]=json!(next); result["has_more"]=json!(next<count);
            json_output(result)
        }
        LocalAction::SearchFiles { pattern,case_sensitive,recursive,.. }=> {
            ensure!(!pattern.is_empty() && pattern.len()<=4000,"Search text must contain 1 to 4000 bytes");
            let pattern=if *case_sensitive { pattern.clone() } else { pattern.to_lowercase() };
            let mut pending=vec![path.to_path_buf()]; let mut matches=Vec::new(); let mut skipped=0; let mut visited=0; let mut used=256; let mut limited=false;
            while let Some(path)=pending.pop() {
                ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Search stopped");
                if visited>=10_000 || matches.len()>=100 || limited { pending.push(path); break; }
                visited+=1;
                let metadata=match fs::symlink_metadata(&path) { Ok(metadata)=>metadata,Err(_)=> { skipped+=1; continue; } };
                if metadata.is_symlink() { skipped+=1; continue; }
                if metadata.is_dir() {
                    let entries=match fs::read_dir(&path) { Ok(entries)=>entries,Err(_)=> { skipped+=1; continue; } };
                    for (index,entry) in entries.take(10_001).enumerate() {
                        if index>=10_000 || pending.len()>=10_000 { skipped+=1; break; }
                        let entry=match entry { Ok(entry)=>entry,Err(_)=> { skipped+=1; continue; } };
                        if *recursive || entry.file_type().is_ok_and(|kind|kind.is_file()) { pending.push(entry.path()); }
                    }
                    continue;
                }
                if !metadata.is_file() || metadata.len()>16*1024*1024 { skipped+=1; continue; }
                let file=match open_file(&path) { Ok(file)=>file,Err(_)=> { skipped+=1; continue; } };
                let mut reader=BufReader::new(file); let mut line=Vec::new(); let mut number=0; let mut offset=0;
                loop {
                    ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Search stopped");
                    line.clear();
                    let count=match reader.by_ref().take(64*1024+1).read_until(b'\n',&mut line) { Ok(count)=>count,Err(_)=> { skipped+=1; break; } };
                    if count==0 { break; }
                    number+=1; let start=offset; offset+=count;
                    if line.len()>64*1024 { skipped+=1; break; }
                    let text=match std::str::from_utf8(&line) { Ok(text) if !text.contains('\0')=>text, _=> { skipped+=1; break; } };
                    let found=if *case_sensitive { text.contains(&pattern) } else { text.to_lowercase().contains(&pattern) };
                    if found {
                        let item=json!({"path":path,"line":number,"offset":start,"text":text.chars().take(1000).collect::<String>()});
                        let bytes=item.to_string().len()+1;
                        if used+bytes>protocol::TOOL_OUTPUT_BYTES { limited=true; break; }
                        used+=bytes; matches.push(item);
                        if matches.len()>=100 { limited=true; break; }
                    }
                }
            }
            json_output(json!({"matches":matches,"visited":visited,"skipped":skipped,"truncated":limited || !pending.is_empty() || skipped>0}))
        }
        LocalAction::ApplyPatch { old_text,new_text,.. }=> {
            ensure!(!old_text.is_empty(),"Replacement text must identify a nonempty exact block");
            let target=fs::canonicalize(path)?; let mut source=open_file(&target)?; let metadata=source.metadata()?;
            ensure!(metadata.is_file() && metadata.len()<=16*1024*1024,"Editing requires a text file up to 16 MiB");
            OpenOptions::new().write(true).open(&target)?;
            #[cfg(unix)] { use std::os::unix::fs::MetadataExt; ensure!(metadata.nlink()==1,"Atomic replacement would break hard links; edit this file through an explicitly approved command instead"); }
            let mut original=Vec::new(); Read::by_ref(&mut source).take(16*1024*1024+1).read_to_end(&mut original)?;
            ensure!(original.len()<=16*1024*1024,"File grew beyond the edit limit");
            let text=std::str::from_utf8(&original).context("File is not UTF-8 text")?;
            ensure!(text.matches(old_text).count()==1,"The old text must match exactly once; read the current file and narrow the replacement");
            let replacement=text.replacen(old_text,new_text,1);
            let mut temporary=tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
            temporary.write_all(replacement.as_bytes())?;
            metadata::preserve(&source,&temporary)?;
            temporary.as_file().sync_all()?;
            let mut current=Vec::new(); open_file(&target)?.take(16*1024*1024+1).read_to_end(&mut current)?;
            ensure!(current==original,"File changed while preparing the edit; read it again");
            ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Edit stopped before commit");
            drop(source);
            metadata::replace(temporary,&target)?;
            Ok(format!("Edited {}",path.display()))
        }
        LocalAction::WriteFile { content,.. }=> {
            let mut builder=tempfile::Builder::new(); builder.prefix(".assistant-");
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; builder.permissions(fs::Permissions::from_mode(0o666)); }
            let mut file=builder.tempfile_in(path.parent().context("Missing parent directory")?)?;
            file.write_all(content.as_bytes())?; file.as_file().sync_all()?;
            ensure!(!stopped.load(std::sync::atomic::Ordering::SeqCst),"Creation stopped before commit");
            file.persist_noclobber(path)?;
            Ok(format!("Created {}",path.display()))
        }
        _=>unreachable!(),
    }
}

/// Reject devices and pipes before reading; nonblocking Unix open also closes the FIFO replacement race.
pub(crate) fn open_file(path:&Path)->Result<File> {
    ensure!(fs::metadata(path)?.is_file(),"Only regular files can be read by this tool");
    let mut options=OpenOptions::new(); options.read(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.custom_flags(libc::O_NONBLOCK|libc::O_NOCTTY); }
    let file=options.open(path)?;
    ensure!(file.metadata()?.is_file(),"Only regular files can be read by this tool");
    Ok(file)
}

/// Budget serialized JSON, advancing the cursor only over text actually returned to the model.
fn read_file(path:&Path,offset:u64,limit:u64)->Result<String> {
    let mut file=open_file(path)?; let total=file.metadata()?.len();
    file.seek(SeekFrom::Start(offset))?;
    let limit=limit.clamp(4,24_000) as usize;
    let mut bytes=Vec::new(); file.take(limit as u64+4).read_to_end(&mut bytes)?;
    let start=bytes.iter().position(|byte|byte & 0xc0!=0x80).unwrap_or(bytes.len());
    let mut end=limit.min(bytes.len());
    ensure!(start<=end && start<=3 && (offset>0 || start==0),"File contains invalid UTF-8");
    if let Err(error)=std::str::from_utf8(&bytes[start..end]) {
        if error.error_len().is_none() && bytes.len()>end { end=start+error.valid_up_to(); }
    }
    let text=std::str::from_utf8(&bytes[start..end]).context("File is not UTF-8 text")?;
    ensure!(!text.contains('\0'),"File contains binary data");
    let mut keep=text.len();
    loop {
        let result=json!({"path":path,"offset":offset+start as u64,"next_offset":offset+start as u64+keep as u64,"total_bytes":if total==0 && !bytes.is_empty() { None } else { Some(total) },"eof":keep==text.len() && end==bytes.len(),"content":&text[..keep]});
        let encoded=result.to_string();
        if encoded.len()<=protocol::TOOL_OUTPUT_BYTES { return Ok(encoded); }
        ensure!(keep>0,"File metadata exceeds the output budget");
        keep=keep.saturating_sub((encoded.len()-protocol::TOOL_OUTPUT_BYTES).max(1));
        while !text.is_char_boundary(keep) { keep-=1; }
    }
}

/// Complete records are omitted at collection time instead of cutting JSON in the middle of an item.
fn json_output(value:serde_json::Value)->Result<String> {
    let encoded=value.to_string();
    ensure!(encoded.len()<=protocol::TOOL_OUTPUT_BYTES,"File result metadata exceeds the output budget");
    Ok(encoded)
}
