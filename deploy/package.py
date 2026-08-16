#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
iLink-WM1 本地打包脚本（对齐 v3.2.3 轮次的分发目录结构）

用法（项目根目录执行）：
    python deploy/package.py            # 打 src.zip + win_x64.zip（需先 cargo build --release）
    python deploy/package.py --skip-exe # 仅打源码包

产出（写入 分发/）：
    ilink_wm_v<版本>_src.zip
    ilink_wm_v<版本>_win_x64.zip
    SHA256SUMS.txt
"""

import hashlib
import re
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "分发"


def read_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    if not m:
        sys.exit("[package] 无法从 Cargo.toml 解析版本号")
    return m.group(1)


def add_dir(zf: zipfile.ZipFile, src_dir: Path, names=None):
    for p in sorted(src_dir.rglob("*")):
        if p.is_file():
            if names is not None and p.name not in names:
                continue
            zf.write(p, p.relative_to(src_dir.parent).as_posix())


def add_file(zf: zipfile.ZipFile, path: Path, arcname: str = None):
    if path.is_file():
        zf.write(path, arcname or path.name)


def main():
    ver = read_version()
    skip_exe = "--skip-exe" in sys.argv
    DIST.mkdir(exist_ok=True)

    # ── 源码包 ────────────────────────────────────────────
    src_zip = DIST / f"ilink_wm_v{ver}_src.zip"
    with zipfile.ZipFile(src_zip, "w", zipfile.ZIP_DEFLATED) as zf:
        add_dir(zf, ROOT / "src")
        add_dir(zf, ROOT / "web")
        for f in ("Cargo.toml", "Cargo.lock", "LICENSE", "README.md", "CHANGELOG.md",
                  "start.bat", "install-service.bat", "代码规范.md", "用户协议.md", "部署指南.md"):
            add_file(zf, ROOT / f)
        # 服务器部署脚本以包内 install.sh 身份随行（同 3.2.3 轮次）
        add_file(zf, ROOT / "deploy" / "linux" / "install-server.sh", "install.sh")
    print(f"[package] {src_zip.name}（{src_zip.stat().st_size/1024:.0f} KB）")

    # ── Windows 二进制包 ──────────────────────────────────
    if not skip_exe:
        exe = ROOT / "target" / "release" / "ilink-wm1.exe"
        if not exe.is_file():
            sys.exit("[package] 未找到 target/release/ilink-wm1.exe，请先 cargo build --release")
        win_zip = DIST / f"ilink_wm_v{ver}_win_x64.zip"
        with zipfile.ZipFile(win_zip, "w", zipfile.ZIP_DEFLATED) as zf:
            add_dir(zf, ROOT / "web")
            add_file(zf, exe, "ilink-wm1.exe")
            for f in ("LICENSE", "README.md", "CHANGELOG.md", "start.bat",
                      "install-service.bat", "用户协议.md", "部署指南.md"):
                add_file(zf, ROOT / f)
        print(f"[package] {win_zip.name}（{win_zip.stat().st_size/1024/1024:.1f} MB）")

    # ── SHA-256 清单 ──────────────────────────────────────
    manifest = DIST / "SHA256SUMS.txt"
    with manifest.open("w", encoding="utf-8", newline="\n") as f:
        for z in sorted(DIST.glob("*.zip")):
            h = hashlib.sha256(z.read_bytes()).hexdigest()
            f.write(f"{h}  {z.name}\n")
    print(f"[package] 校验清单 → {manifest}")


if __name__ == "__main__":
    main()
