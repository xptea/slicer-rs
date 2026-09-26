#!/usr/bin/env python3
"""Build the cached minimal Linux playback sources with required NVIDIA decode.

Requires build/mpv-toolchain/{src,sysroot}, including the pinned nv-codec-headers
archive. Produces build/mpv-toolchain/nvdec/mpv-build/libmpv.so.2.5.0. Does not
modify an installed application or bundle. See docs/multitrack.md.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile

root = Path(__file__).resolve().parent.parent
tc = root / "build/mpv-toolchain"
out = tc / "nvdec"
stage = out / "stage"
sysroot = tc / "sysroot"
profile = json.loads((root / "packaging/playback-nvdec.json").read_text())
archive = tc / "src/nv-codec-headers-13.0.19.0.tar.gz"
if hashlib.sha256(archive.read_bytes()).hexdigest() != profile["nvcodec_sha256"]:
    raise SystemExit("NVIDIA header archive checksum mismatch")
out.mkdir(exist_ok=True)
with tarfile.open(archive) as source:
    source.extractall(out, filter="data")
subprocess.run(["make", "install", f"PREFIX={stage}"], cwd=out / "nv-codec-headers-13.0.19.0", check=True)
env = os.environ.copy()
env["PKG_CONFIG_PATH"] = str(stage / "lib/pkgconfig")
ffbuild = out / "ffmpeg-build"
ffbuild.mkdir(exist_ok=True)
subprocess.run([str(tc / "src/ffmpeg-7.1.5/configure"), *profile["ffmpeg"],
    f"--prefix={stage}", "--enable-ffnvcodec", "--enable-cuda", "--enable-nvdec",
    "--enable-hwaccel=h264_nvdec", "--enable-hwaccel=hevc_nvdec",
    "--enable-hwaccel=av1_nvdec", "--enable-hwaccel=vp9_nvdec"], cwd=ffbuild, env=env, check=True)
subprocess.run(["make", "-j8"], cwd=ffbuild, env=env, check=True)
subprocess.run(["make", "install"], cwd=ffbuild, env=env, check=True)
env["PYTHONPATH"] = str(sysroot / "usr/lib/python3/dist-packages")
env["PATH"] = str(sysroot / "usr/bin") + os.pathsep + env["PATH"]
env["PKG_CONFIG_PATH"] = os.pathsep.join(map(str, [stage / "lib/pkgconfig",
    sysroot / "usr/lib/x86_64-linux-gnu/pkgconfig", Path("/usr/lib/x86_64-linux-gnu/pkgconfig"), Path("/usr/share/pkgconfig")]))
env["CFLAGS"] = f"-I{sysroot}/usr/include"
# Hide static FFmpeg 7 symbols so the host's FFmpeg 8 cannot interpose its ABI.
env["LDFLAGS"] = f"-L{stage}/lib -L{sysroot}/usr/lib/x86_64-linux-gnu -Wl,--exclude-libs,ALL -Wl,-rpath,{root}/build/playback-minimal/linux-x86_64"
mpbuild = out / "mpv-build"
setup = ["meson", "setup"]
if (mpbuild / "build.ninja").exists():
    setup.append("--reconfigure")
subprocess.run([*setup, str(mpbuild), str(tc / "src/mpv-0.41.0"), *profile["mpv"],
    "-Dcuda-hwaccel=enabled", "-Dcuda-interop=enabled"], env=env, check=True)
subprocess.run(["ninja", "-C", str(mpbuild), "-j8"], env=env, check=True)
config = (mpbuild / "config.h").read_text()
assert "#define HAVE_CUDA_HWACCEL 1" in config
assert "#define HAVE_CUDA_INTEROP 1" in config
print(f"Built NVDEC playback runtime: {mpbuild / 'libmpv.so.2.5.0'}")
