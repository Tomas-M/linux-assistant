#!/usr/bin/env python3
"""Create a small server source bundle without local tools, build output, or credentials."""
import io
from pathlib import Path
import subprocess
import tarfile
import time

ROOT=Path(__file__).resolve().parents[1]


# Include only application source and deployment files; pin Codex from the clean checkout.
def main():
    upstream=ROOT/"vendor/codex"
    revision=subprocess.check_output(["git","rev-parse","HEAD"],cwd=upstream,text=True).strip()
    if subprocess.check_output(["git","status","--porcelain"],cwd=upstream,text=True).strip():
        raise RuntimeError("Codex has local changes that would be missing from the download.")
    files=[ROOT/name for name in (
        "Cargo.toml","Cargo.lock","rust-toolchain.toml",".cargo/config.toml",
        "README.md","THIRD_PARTY_NOTICES.md","deploy/START-HERE.md",
        "deploy/server.env.example","deploy/linux-assistant.service","scripts/build_server.sh","scripts/install_server_update.sh",
        "scripts/reset_conversations.sh","scripts/reset_conversations.py")]
    files+=sorted(path for path in (ROOT/"crates").rglob("*")
                  if path.is_file() and path.suffix in (".rs",".toml",".sql",".html",".css",".js"))
    output=ROOT/"dist/linux-assistant-server-source.tar.gz"
    output.parent.mkdir(exist_ok=True)
    # Fresh timestamps make Cargo rebuild changed sources when a bundle replaces an older one.
    timestamp=int(time.time())
    with tarfile.open(output,"w:gz",compresslevel=9) as archive:
        for path in files:
            if path.is_symlink():
                raise RuntimeError(f"Refusing to package a source symlink: {path}")
            data=path.read_bytes()
            if path.suffix==".sh":
                data=data.replace(b"\r\n",b"\n")
            name="START-HERE.md" if path.name=="START-HERE.md" else path.relative_to(ROOT).as_posix()
            entry=tarfile.TarInfo("linux-assistant/"+name)
            entry.size=len(data)
            entry.mtime=timestamp
            entry.mode=0o755 if path.suffix==".sh" else 0o644
            archive.addfile(entry,io.BytesIO(data))
        data=(revision+"\n").encode("ascii")
        entry=tarfile.TarInfo("linux-assistant/CODEX_REVISION")
        entry.size=len(data)
        entry.mtime=timestamp
        archive.addfile(entry,io.BytesIO(data))
    print(f"Created {output} ({output.stat().st_size:,} bytes); Codex {revision}")


if __name__=="__main__":
    main()
