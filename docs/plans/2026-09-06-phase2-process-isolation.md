# 下一阶段 · 进程隔离（Phase 2）

承接 `744ffb4e`（轻量化 + 模型菜单）。本阶段目标：可选服务不再跟第一帧抢 CPU/端口。

## 切片

| ID | 内容 | 状态 |
|----|------|------|
| P2-1 | 内嵌浏览器 MCP 按需 `ensure_started`（connect / 打开 Browser） | 已做 |
| P2-2 | Browser/Terminal 标签按需加载 WebView/xterm 模块 | 已做（独立窗口未做） |
| P2-3 | Remote IM sidecar（关着零进程） | 未做 |
| P2-4 | 聊天线程 ConversationThreadLive 按需加载 | 已做 |
| P2-5 | session API 推迟 2.5s 再 bind | 已做 |

## 不做

- 不迁 Electron
- 不把 grok CLI 打进安装包
