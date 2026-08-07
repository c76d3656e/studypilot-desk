# Changelog

本项目遵循语义化版本。版本 PR、标签和 GitHub Release 由 Release Please 根据 Conventional Commits 自动维护。

## 0.2.0

- 将桌面运行时从 Electron 迁移到 Tauri 2。
- 使用 Rust Actix-Web 作为唯一桌面 HTTP 调度入口。
- 将 Python 收口为不监听端口的私有 stdio Worker。
