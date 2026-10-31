#!/usr/bin/env python3
"""Run backend Cargo commands with the pinned Codex V8 artifacts and checksums."""
import os
from pathlib import Path
import subprocess
import sys

ROOT=Path(__file__).resolve().parents[1]


# Reuse upstream's verified downloads instead of maintaining our own V8 build or mirror.
def environment(arguments=()):
    os.environ["CODEX_REPO_ROOT"]=str(ROOT/"vendor/codex")
    sys.path.insert(0,str(ROOT/"vendor/codex/scripts"))
    from codex_package.targets import TARGET_SPECS
    from codex_package.v8 import resolve_codex_v8_cargo_env
    target=os.environ.get("CARGO_BUILD_TARGET")
    for index,argument in enumerate(arguments):
        if argument=="--target": target=arguments[index+1]
        elif argument.startswith("--target="): target=argument.split("=",1)[1]
    if not target:
        version=subprocess.check_output(["rustc","-vV"],cwd=ROOT,text=True)
        target=next(line.removeprefix("host: ") for line in version.splitlines() if line.startswith("host: "))
    return dict(os.environ,**resolve_codex_v8_cargo_env(TARGET_SPECS[target],cache_root=ROOT/".tools/v8"))


if __name__=="__main__":
    sys.exit(subprocess.call(["cargo",*sys.argv[1:]],cwd=ROOT,env=environment(sys.argv[1:])))
