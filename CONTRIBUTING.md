# 贡献指南

感谢您考虑贡献 iLink-WM1！本项目以 Apache-2.0 开源，欢迎 PR。

## 提交流程

1. **Fork** 本仓库
2. 创建 feature branch：
   ```bash
   git checkout -b feat/your-feature-name
   # 或 fix/xxx  refactor/xxx  docs/xxx
   ```
3. 编写代码 + 同步更新 `CHANGELOG.md`（如属于用户可见的改动）
4. 跑本地检查：
   ```bash
   cargo fmt --all
   cargo clippy --all-targets --locked -- -D warnings
   cargo test --all-targets --locked
   cargo deny check
   cargo build --release --locked
   ```
5. **Commit message** 用 [Conventional Commits](https://www.conventionalcommits.org/) 风格：
   ```
   feat(bot): 支持消息撤回
   fix(web): 修复 ChatView 滚动卡顿
   refactor(storage): 拆分 user.db 表结构
   docs(readme): 更新部署章节
   chore(ci): 升级 rust-cache 到 v3
   deps: bump axum to 0.8
   ```
6. 推送 + 开 PR，PR 标题与 commit message 同风格
7. 等待 CI 通过 + 至少 1 个 review approval 后合入

## 提 Issue

- 🐛 **Bug 报告**：用 [.github/ISSUE_TEMPLATE/bug_report.md](.github/ISSUE_TEMPLATE/bug_report.md)
- 💡 **功能建议**：用 [.github/ISSUE_TEMPLATE/feature_request.md](.github/ISSUE_TEMPLATE/feature_request.md)
- 🔒 **安全漏洞**：见 [SECURITY.md](SECURITY.md) 的私密报告渠道（**勿公开 Issue**）

## 本地构建

- Rust 工具链：仓库 `rust-toolchain.toml` 固定的 Rust 1.95.0（`rustup toolchain install 1.95.0`）
- Windows：`rustup default stable-x86_64-pc-windows-msvc` + 安装 Build Tools
- 依赖：项目根目录 `cargo build --release`
- 路径含中文时 Cargo 可能乱码，设 `CARGO_TARGET_DIR=C:\ilink-wm1-target`
- 也可以用 `python run_test.py`（项目附带的开发辅助脚本，会自动复制 `web/` 到 target 目录）

## 提交规范要点

- 改动 `src/` 内的 Rust 代码必须 `cargo fmt` + `cargo clippy` 通过
- 改动 `web/` 内的前端代码无需特别 lint，但保持现有命名风格（zn-*.js 模块化）
- 改动 `deploy/` 内安装脚本，必须在 Windows / Linux / macOS 三平台至少一平台实测
- 不引入与现有依赖重复的新 crate（先确认 `Cargo.toml` 现有依赖能否实现）
- 公共 API 变更需同步更新 `CHANGELOG.md` "变更"或"破坏性"段

## 维护者联系

- GitHub: [@Wong0728](https://github.com/Wong0728)
- 安全披露：[SECURITY.md](SECURITY.md) 列出的私密渠道

再次感谢您的贡献！
