"""Builds an opkg .ipk: an ar archive of debian-binary, control.tar.gz and data.tar.gz, owned by root.

python device/build-ipk.py <package folder> -o <out.ipk>
The folder holds control/ (control, plus preinst/postinst/prerm/postrm) and, optionally, data/ (the
files to install, laid out from /).
"""

import argparse
import gzip
import io
import tarfile
import time
from pathlib import Path

SCRIPTS = {"preinst", "postinst", "prerm", "postrm"}


def is_script(path: Path) -> bool:
    """opkg scripts, .sh files and anything starting with #!, which need Unix line endings."""
    if path.name in SCRIPTS or path.suffix == ".sh":
        return True
    with path.open("rb") as f:
        return f.read(2) == b"#!"


def is_elf(path: Path) -> bool:
    with path.open("rb") as f:
        return f.read(4) == b"ELF"


def ar_member(name: str, payload: bytes, mtime: int) -> bytes:
    header = f"{name:<16}{mtime:<12}{0:<6}{0:<6}{0o100644:<8o}{len(payload):<10}`\n".encode("ascii")
    assert len(header) == 60, f"ar header for {name} is {len(header)} bytes"
    return header + payload + (b"\n" if len(payload) % 2 else b"")


def tar_gz(folder: Path | None, mtime: int) -> bytes:
    buffer = io.BytesIO()
    with gzip.GzipFile(fileobj=buffer, mode="wb", mtime=mtime) as gz, tarfile.open(fileobj=gz, mode="w") as tar:
        paths = sorted(folder.rglob("*")) if folder and folder.is_dir() else []
        for path in paths:
            info = tar.gettarinfo(str(path), arcname=f"./{path.relative_to(folder).as_posix()}")
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            info.mtime = mtime
            # Windows checkouts carry no executable bit; scripts and anything under a bin folder need one.
            script = path.is_file() and is_script(path)
            if path.is_file() and (script or is_elf(path) or path.parent.name in {"bin", "init.d"}):
                info.mode = 0o755
            if path.is_file():
                data = path.read_bytes().replace(b"\r\n", b"\n") if script else path.read_bytes()
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
            else:
                tar.addfile(info)
    return buffer.getvalue()


def main() -> None:
    parser = argparse.ArgumentParser(description="Build an opkg .ipk package")
    parser.add_argument("package", type=Path, help="folder with control/ and optionally data/")
    parser.add_argument("-o", "--output", type=Path, required=True)
    args = parser.parse_args()
    control = args.package / "control"
    if not (control / "control").is_file():
        raise SystemExit(f"{control / 'control'} is missing")
    mtime = int(time.time())
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("wb") as out:
        out.write(b"!<arch>\n")
        out.write(ar_member("debian-binary", b"2.0\n", mtime))
        out.write(ar_member("control.tar.gz", tar_gz(control, mtime), mtime))
        out.write(ar_member("data.tar.gz", tar_gz(args.package / "data", mtime), mtime))
    print(f"Built {args.output} ({args.output.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
