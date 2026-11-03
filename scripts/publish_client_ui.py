#!/usr/bin/env python3
"""Publish one atomic, versioned browser UI release without compiling Rust."""
import argparse
import base64
import hashlib
import json
import mimetypes
import os
from pathlib import Path
import tempfile

ROOT=Path(__file__).resolve().parents[1]


# Readers keep the previous complete release until the atomic replace succeeds.
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output",type=Path,default=Path("/usr/local/lib/linux-assistant/client-ui.json"))
    args=parser.parse_args()
    files={}
    for path in sorted((ROOT/"client-ui").rglob("*")):
        if not path.is_file() or path.name.endswith(".md"):
            continue
        if path.is_symlink():
            raise ValueError(f"UI assets must not be symlinks: {path}")
        data=path.read_bytes()
        if path.name=="index.html":
            data=data.replace(b"{{ICONS}}",(ROOT/"crates/protocol/assets/icons.svg").read_bytes())
        content_type={".js":"text/javascript; charset=utf-8",".css":"text/css; charset=utf-8",".html":"text/html; charset=utf-8",".svg":"image/svg+xml"}.get(path.suffix,mimetypes.guess_type(path.name)[0] or "application/octet-stream")
        files["/"+path.relative_to(ROOT/"client-ui").as_posix()]={"content_type":content_type,"content":base64.b64encode(data).decode("ascii")}
    digest=hashlib.sha256()
    for name,asset in sorted(files.items()):
        digest.update((name+"\0"+asset["content_type"]+"\0"+asset["content"]+"\0").encode("utf-8"))
    bundle={"bridge_api":1,"version":digest.hexdigest(),"files":files}
    data=json.dumps(bundle,separators=(",",":"),ensure_ascii=False).encode("utf-8")
    if len(data)>8*1024*1024:
        raise ValueError("Client UI bundle exceeds 8 MiB")
    args.output.parent.mkdir(parents=True,exist_ok=True)
    descriptor,staged=tempfile.mkstemp(prefix=".client-ui-",dir=args.output.parent)
    try:
        with os.fdopen(descriptor,"wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(staged,0o644)
        os.replace(staged,args.output)
    finally:
        if os.path.exists(staged):
            os.unlink(staged)
    print(f"Published client UI {bundle['version'][:12]} to {args.output}")


if __name__=="__main__":
    main()
