#!/usr/bin/env python3
"""Build the `dre-cli` wheels for PyPI from a release's assets.

One platform wheel per OS/arch, like ruff and uv ship: the `dre` executable, a `dre` console
command, `python -m dre_cli`, and a small Python API (`dre_cli.run([...])`). Nothing is compiled
here; the wheels repackage the release's binaries. Plugins aren't in the wheel: `dre` installs
the ones a project declares on demand, as it does however it was installed.

    build_wheels.py --dist dist --version 0.0.1-alpha-3 --out wheels   # the release workflow
    build_wheels.py --from-dir target/release --version 0.0.1-alpha-3   # a local build, this machine

The PyPI name `dre` is taken by an unrelated project, so the distribution is `dre-cli`; the
command is still `dre`. Versions map to PEP 440: 0.0.1-alpha-3 -> 0.0.1a3.
"""

import argparse
import base64
import hashlib
import json
import platform as pyplatform
import re
import stat
import sys
import tarfile
import zipfile
from pathlib import Path

REPO = "get-dre/dre"
DIST_NAME = "dre-cli"
PKG = "dre_cli"

# Release platform -> wheel platform tag. The Linux builds run on Ubuntu 22.04 (glibc 2.35),
# which is what Databricks serverless (aarch64) and recent Databricks Runtimes have.
PLATFORMS = {
    "linux-x86_64": "manylinux_2_35_x86_64",
    "linux-aarch64": "manylinux_2_35_aarch64",
    "macos-aarch64": "macosx_11_0_arm64",
    "macos-x86_64": "macosx_10_12_x86_64",
    "windows-x86_64": "win_amd64",
    "windows-aarch64": "win_arm64",
}


def this_platform():
    os_ = {"Darwin": "macos", "Linux": "linux", "Windows": "windows"}[pyplatform.system()]
    arch = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64", "AMD64": "x86_64"}[pyplatform.machine()]
    return f"{os_}-{arch}"


def pep440(version):
    """0.0.1-alpha-3 -> 0.0.1a3, 1.2.0-rc-1 -> 1.2.0rc1, 1.2.0 -> 1.2.0."""
    m = re.fullmatch(r"(\d+\.\d+\.\d+)(?:-(alpha|beta|rc)(?:[-.]?(\d+))?)?", version)
    if not m:
        sys.exit(f"can't map version {version} to PEP 440")
    base, pre, n = m.groups()
    return base + ({"alpha": "a", "beta": "b", "rc": "rc"}[pre] + (n or "0") if pre else "")


def member(archive, name, exe):
    """The executable `name` (or name.exe) out of a .tar.gz or .zip."""
    wanted = name + exe
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as z:
            for n in z.namelist():
                if Path(n).name == wanted:
                    return z.read(n)
    else:
        with tarfile.open(archive) as t:
            for m in t.getmembers():
                if Path(m.name).name == wanted and m.isfile():
                    return t.extractfile(m).read()
    sys.exit(f"{archive.name} has no {wanted}")


SHIM = '''"""DRE, the Declarative Reporting Engine, as a Python package.

The package holds the `dre` executable; `dre` (the console command) runs it. Plugins aren't
bundled: `dre` installs the ones a project declares on demand.
"""

import os
import subprocess
import sys
from pathlib import Path

__version__ = "{version}"
DRE_VERSION = "{dre_version}"

_HERE = Path(__file__).resolve().parent
_EXE = ".exe" if os.name == "nt" else ""


def executable() -> str:
    """Path to the bundled `dre` executable."""
    return str(_HERE / "bin" / ("dre" + _EXE))


def run(args, **kwargs):
    """Run `dre <args>` and return the CompletedProcess (for Python callers, e.g. a Databricks job)."""
    return subprocess.run([executable(), *args], **kwargs)


def main():
    exe = executable()
    if os.name == "nt":
        sys.exit(subprocess.run([exe, *sys.argv[1:]]).returncode)
    os.execv(exe, [exe, *sys.argv[1:]])
'''

MAIN = "from dre_cli import main\n\nmain()\n"

README = """# dre-cli

[DRE](https://github.com/{repo}), the Declarative Reporting Engine: SQL in, formatted files out.
DRE runs SQL reports and writes csv, delimited, fixed-width, parquet or xlsx files, then delivers
them (S3, GCS, Azure Blob, SFTP/FTP, Databricks Volumes and workspace files, email, Slack).

```bash
pip install dre-cli        # or: uv tool install dre-cli
dre --help
```

This package holds the `dre` {dre_version} executable. Plugins (databases, file formats,
destinations) are installed by `dre` on demand, for the ones a project declares. From Python:
`dre_cli.run(["run", "-s", "daily"])`.

In a Databricks job, add `dre-cli` to the job's environment dependencies and run `dre` (or
`python -m dre_cli`) from a script or notebook. See the
[README](https://github.com/{repo}#readme) for everything else.
"""


def record_line(path, data):
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{path},sha256={digest},{len(data)}"


def build(dre_version, plat, out_dir, dist=None, from_dir=None):
    version = pep440(dre_version)
    wheel_tag = PLATFORMS[plat]
    exe = ".exe" if plat.startswith("windows") else ""
    ext = ".zip" if plat.startswith("windows") else ".tar.gz"

    files = {}  # path in wheel -> (bytes, executable?)
    if from_dir:
        files[f"{PKG}/bin/dre{exe}"] = ((Path(from_dir) / f"dre{exe}").read_bytes(), True)
    else:
        core = Path(dist) / f"dre-{dre_version}-{plat}{ext}"
        files[f"{PKG}/bin/dre{exe}"] = (member(core, "dre", exe), True)
    # The install receipt next to the binary, in place of the release archive's: `dre system
    # update` reads it and points at pip / uv / pipx instead of replacing the binary.
    receipt = {"schema": 1, "source": "pypi", "version": dre_version}
    files[f"{PKG}/bin/dre-receipt.json"] = ((json.dumps(receipt) + "\n").encode(), False)
    shim = SHIM.format(version=version, dre_version=dre_version)
    files[f"{PKG}/__init__.py"] = (shim.encode(), False)
    files[f"{PKG}/__main__.py"] = (MAIN.encode(), False)

    dist_info = f"{PKG}-{version}.dist-info"
    metadata = f"""Metadata-Version: 2.1
Name: {DIST_NAME}
Version: {version}
Summary: DRE, the Declarative Reporting Engine: SQL in, formatted files out
Home-page: https://github.com/{REPO}
Project-URL: Source, https://github.com/{REPO}
License: GPL-3.0-only
Classifier: License :: OSI Approved :: GNU General Public License v3 (GPLv3)
Classifier: Development Status :: 2 - Pre-Alpha
Requires-Python: >=3.8
Description-Content-Type: text/markdown

""" + README.format(repo=REPO, dre_version=dre_version)
    wheel = f"""Wheel-Version: 1.0
Generator: dre build_wheels.py
Root-Is-Purelib: false
Tag: py3-none-{wheel_tag}
"""
    files[f"{dist_info}/METADATA"] = (metadata.encode(), False)
    files[f"{dist_info}/WHEEL"] = (wheel.encode(), False)
    files[f"{dist_info}/entry_points.txt"] = (b"[console_scripts]\ndre = dre_cli:main\n", False)
    files[f"{dist_info}/top_level.txt"] = (f"{PKG}\n".encode(), False)

    out_dir.mkdir(parents=True, exist_ok=True)
    out = out_dir / f"{PKG}-{version}-py3-none-{wheel_tag}.whl"
    record = [record_line(p, d) for p, (d, _) in files.items()] + [f"{dist_info}/RECORD,,"]
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for p, (data, is_exe) in files.items():
            zi = zipfile.ZipInfo(p, date_time=(2026, 1, 1, 0, 0, 0))
            zi.external_attr = ((0o755 if is_exe else 0o644) | stat.S_IFREG) << 16
            zi.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(zi, data, compresslevel=9)
        zi = zipfile.ZipInfo(f"{dist_info}/RECORD", date_time=(2026, 1, 1, 0, 0, 0))
        zi.external_attr = (0o644 | stat.S_IFREG) << 16
        z.writestr(zi, "\n".join(record) + "\n")
    size = out.stat().st_size
    print(f"{plat}: {out.name} ({size / 1e6:.1f} MB)")
    # PyPI refuses files over 100 MB unless the project's limit is raised.
    if size > 100 * 1000 * 1000:
        sys.exit(f"{out.name} is over PyPI's 100 MB limit")
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--version", required=True, help="DRE's version, e.g. 0.0.1-alpha-3")
    ap.add_argument("--dist", help="folder of the release's assets (dre-*.tar.gz / .zip)")
    ap.add_argument("--from-dir", help="a local build (dre and dre-plugin-* in this folder), for this machine")
    ap.add_argument("--platform", action="append", help="release platform (repeatable); default: all in --dist")
    ap.add_argument("--out", default="wheels", help="where to write the wheels (default: wheels)")
    a = ap.parse_args()
    if bool(a.dist) == bool(a.from_dir):
        sys.exit("give one of --dist or --from-dir")
    out = Path(a.out)
    if a.from_dir:
        plats = [this_platform()]
    elif a.platform:
        plats = a.platform
    else:
        plats = [p for p in PLATFORMS if any(Path(a.dist).glob(f"dre-{a.version}-{p}.*"))]
    if not plats:
        sys.exit(f"no dre-{a.version}-<platform> archives in {a.dist}")
    for p in plats:
        if p not in PLATFORMS:
            sys.exit(f"unknown platform {p}; one of {', '.join(PLATFORMS)}")
        build(a.version, p, out, a.dist, a.from_dir)


if __name__ == "__main__":
    main()
