"""
编译并启动 ilink-wm1 (Rust 版) 测试脚本

用法:
    python run_test.py            # Debug 编译 + 启动
    python run_test.py --release  # Release 编译 + 启动
    python run_test.py --build-only  # 仅编译，不启动
"""

import os
import sys
import subprocess
import shutil

# ── 路径配置 ───────────────────────────────────────────────
RUST_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.dirname(RUST_DIR)

# 因为项目路径含中文，设置纯 ASCII 编译输出目录避免 Cargo 问题
CARGO_TARGET_DIR = os.path.join(RUST_DIR, "target")


def find_web_source_dir() -> str:
    """定位前端源 web/ 目录。

    候选顺序（找到第一个含 index.html 或 chat.html 的目录即返回）：
      1. 脚本所在目录的 web/  ← 推荐布局（run_test.py 与 Cargo.toml 同级）
      2. 项目根目录的 web/     ← 兼容历史布局
    """
    candidates = [
        os.path.join(RUST_DIR, "web"),
        os.path.join(PROJECT_ROOT, "web"),
    ]
    for cand in candidates:
        if os.path.isdir(cand) and (
            os.path.isfile(os.path.join(cand, "index.html"))
            or os.path.isfile(os.path.join(cand, "chat.html"))
        ):
            return cand
    # 全部不存在时回退到第一个候选路径，让后续拷贝逻辑给出明确告警
    return candidates[0]


def print_step(msg):
    print(f"\n{'='*60}")
    print(f"  {msg}")
    print(f"{'='*60}")


def build(release: bool) -> tuple:
    """编译项目，返回 (binary_path, target_dir)"""
    if release:
        build_type = "release"
        cargo_args = ["cargo", "build", "--release"]
        target_sub = "release"
    else:
        build_type = "debug"
        cargo_args = ["cargo", "build"]
        target_sub = "debug"

    print_step(f"正在 {build_type} 编译 ...")

    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = CARGO_TARGET_DIR

    try:
        result = subprocess.run(cargo_args, cwd=RUST_DIR, env=env)
    except FileNotFoundError:
        print(f"[错误] 未找到 cargo，请确认 Rust 工具链已安装并在 PATH 中")
        sys.exit(1)
    if result.returncode != 0:
        print(f"[错误] 编译失败，退出码 {result.returncode}")
        sys.exit(1)

    target_dir = os.path.join(CARGO_TARGET_DIR, target_sub)
    binary_path = os.path.join(target_dir, "ilink-wm1.exe")
    if not os.path.exists(binary_path):
        print(f"[错误] 编译产物未找到: {binary_path}")
        sys.exit(1)

    print(f"[OK] 编译成功: {binary_path}")
    return binary_path, target_dir


def ensure_web_files(target_dir: str):
    """同步源 web/ 目录到 target 运行目录（始终覆盖，避免源文件更新后前端白屏）。

    重要：必须先清空目标 web/，再完整复制。如果只做合并，新版源中已删除的
    文件（例如旧的 script.js 被 zn-*.js 模块替代）会残留到目标目录，导致
    index.html/chat.html 引用陈旧的 JS 路径，浏览器白屏。
    """
    web_src = find_web_source_dir()
    target_web = os.path.join(target_dir, "web")
    if not os.path.isdir(web_src):
        print(f"[警告] 前端源目录不存在: {web_src}")
        print(f"       当前 run_test.py 不会复制任何文件，请确认 web/ 目录位置。")
        return
    print_step("同步前端文件到运行目录 ...")
    print(f"  源目录: {web_src}")
    print(f"  目标:   {target_web}")
    try:
        # 先清空目标目录，再完整复制，确保陈旧文件不会残留
        if os.path.isdir(target_web):
            shutil.rmtree(target_web)
        shutil.copytree(web_src, target_web)
        count = sum(len(files) for _, _, files in os.walk(target_web))
        print(f"  [OK] 已同步 {count} 个文件 -> {target_web}")
    except OSError as e:
        print(f"[警告] 复制前端文件失败: {e}（不影响编译，仅网页可能不可用）")
        return


def run_server(target_dir: str):
    """从 target 目录直接启动（保持 PDB 可用）"""
    print_step("正在启动服务器 ...")

    env = os.environ.copy()
    env.setdefault("ILINK_HOST", "127.0.0.1")
    env.setdefault("ILINK_PORT", "8888")
    env["RUST_BACKTRACE"] = "full"
    # 开启 info 级日志，使收发消息日志（[SEND]/[RECV]/[发送成功] 等）在控制台可见
    env.setdefault("RUST_LOG", "ilink_wm1=info")

    print(f"  运行目录: {target_dir}")
    print(f"  监听地址: {env['ILINK_HOST']}:{env['ILINK_PORT']}")
    print(f"  访问地址: http://localhost:{env['ILINK_PORT']}")
    print()
    print("  按 Ctrl+C 停止服务器")
    print()

    exe_path = os.path.join(target_dir, "ilink-wm1.exe")
    try:
        subprocess.run([exe_path], cwd=target_dir, env=env)
    except KeyboardInterrupt:
        print("\n[INFO] 用户中断")
    except FileNotFoundError:
        print(f"[错误] 找不到 {exe_path}，请先编译")
        sys.exit(1)
    except OSError as e:
        print(f"[错误] 启动服务器失败: {e}")
        sys.exit(1)


def main():
    release = False
    build_only = False

    for arg in sys.argv[1:]:
        if arg == "--release":
            release = True
        elif arg == "--build-only":
            build_only = True
        elif arg in ("-h", "--help"):
            print(__doc__.strip())
            return
        else:
            print(f"[错误] 未知参数: {arg}")
            print(__doc__.strip())
            sys.exit(1)

    binary, target_dir = build(release)
    ensure_web_files(target_dir)

    if not build_only:
        run_server(target_dir)
    else:
        print(f"\n[OK] 编译完成")
        print(f"   二进制: {binary}")


if __name__ == "__main__":
    try:
        main()
    except Exception as e:
        print(f"\n[致命错误] {e}")
        sys.exit(1)
