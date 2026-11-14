use sha2::{Digest,Sha256};
use uuid::Uuid;

/// Derive a backend-specific identifier without disclosing the original machine ID.
pub(super) fn for_backend(backend:&str)->Option<String> {
    let (source,id)=machine_id()?;
    let origin=reqwest::Url::parse(backend).ok()?.origin().ascii_serialization();
    Some(format!("{:x}",Sha256::digest(format!("linux-assistant/device/v1\0{origin}\0{source}\0{}",id.simple()).as_bytes())))
}

/// Reject blank and placeholder IDs instead of giving unrelated computers the same fingerprint.
fn valid_id(value:&str)->Option<Uuid> {
    Uuid::parse_str(value.trim()).ok().filter(|id|!id.is_nil() && id.as_u128()!=u128::MAX)
}

/// Use the firmware UUID because cloned Linux images can share installation IDs.
#[cfg(target_os="linux")]
fn machine_id()->Option<(&'static str,Uuid)> {
    let uuid=std::fs::read_to_string("/sys/class/dmi/id/product_uuid").ok()?;
    valid_id(&uuid).map(|id|("linux-dmi",id))
}

/// Read the machine-wide Windows installation ID without starting a shell.
#[cfg(windows)]
fn machine_id()->Option<(&'static str,Uuid)> {
    use windows_sys::{core::w,Win32::System::Registry::{RegGetValueW,HKEY_LOCAL_MACHINE,RRF_RT_REG_SZ,RRF_SUBKEY_WOW6464KEY}};
    let mut value=[0u16;64];
    let mut size=std::mem::size_of_val(&value) as u32;
    // The buffer size is in bytes and the requested registry string is NUL-terminated.
    let status=unsafe { RegGetValueW(HKEY_LOCAL_MACHINE,w!("SOFTWARE\\Microsoft\\Cryptography"),w!("MachineGuid"),
        RRF_RT_REG_SZ|RRF_SUBKEY_WOW6464KEY,std::ptr::null_mut(),value.as_mut_ptr().cast(),&mut size) };
    if status!=0 { return None; }
    let end=value.iter().position(|unit|*unit==0)?;
    valid_id(&String::from_utf16(&value[..end]).ok()?).map(|id|("windows-machine",id))
}
