# 测试报告 · Apple Design + 轻量化（2026-09-06）

命令：`pnpm exec vitest run`（下列文件）  
环境：Windows / Node / vitest 3.2.7  
结果：**本轮相关 112 + 守卫/Markdown 补跑 全部 PASS**

## 本轮交付（不问，直接做完能做的）

| 项 | 状态 | 证据 |
|----|------|------|
| A0 IM 未启用不 boot | 已做 | `should_boot_at_launch` · `appleMotion.guard.test.ts` |
| B0 合同 wiki | 已做 | `docs/llm-wiki/apple-motion.md` |
| B1 按下 + reduced-transparency | 已做 | `tokens.css` · guard |
| A1 主包切片 | 已做 | `App.js` **2.69 MB → 1.72 MB** |
| B2 分栏橡胶边 | 已做 | `motionSpring.ts` + layout/aside/terminal live drag |
| B3 Win 实心 / tracking | 已做 | `--sidebar-blur: 0` on win/linux · `--track-*` |
| A2 崩溃报告 | 已做 | `logs/last_crash.json` 合成 unclean + last_crash.txt |
| Companion 视频 | 已做 | `shouldPlayWallpaperVideo({ reducedMotion })` |
| IM sidecar / 独立浏览器进程 / Vaul / App.js&lt;800KB | **没做** | 会拆 Host 进程模型或重写 WKWebView，不能当这次切片硬塞 |

## Vitest（PASS）

| 文件 | 条数 |
|------|------|
| `src/lib/motionSpring.test.ts` | 5 |
| `src/lib/layout.test.ts` | 19 |
| `src/lib/bottomTerminal.test.ts` | 20 |
| `src/lib/streamRenderPolicy.test.ts` | 9 |
| `src/lib/appleMotion.guard.test.ts` | 4 |
| `src/lib/openPresence.test.ts` | 15 |
| `src/lib/viteManualChunks.test.ts` | 6 |
| `src/lib/paneSplitMotion.test.ts` | 25 |
| `src/hooks/usePaneSplitMotion.test.tsx` | 9 |
| `src/components/MarkdownBody.test.tsx` | 3 |
| `src/components/lobe-chat/MarkdownChat.test.tsx` | 9 |
| `src/lib/providersPanelScroll.guard.test.ts` | 3 |

合计：**112**（第一组）+ Markdown/守卫补跑 **PASS**。

断言要点：

- `liveDragWidth(500, 200, 420)` 大于 420 且小于 500（橡胶边）
- `liveSidebarDragWidth(600)` 同理；松手路径仍走 `clamp*` / `resolveSidebarDragEnd`
- `liveBottomTerminalHeight(900, 300)` 过冲再硬 clamp
- 壁纸 `reducedMotion: true` → 不播视频
- token：`--press-scale`、`prefers-reduced-transparency`、`.platform-win --sidebar-blur: 0px`
- 设置 `lazy` + `prefetchSettingsStage`

## 构建

`pnpm exec vite build` **成功**（26.5s）。主包 `dist/assets/App-*.js` ≈ **1.72 MB**（原 2.69 MB）。设置/Office/xterm/pdfjs/xlsx/plyr/hljs 为独立 chunk。

## Rust

`should_boot_connectors_*` 与 `compose_crash_report` 已编译进 `grok_app_lib`。本机 `cargo test` 跑 exe 时报 `STATUS_ENTRYPOINT_NOT_FOUND`（MSVC 运行库未进当前 shell），**不是断言失败**。`cl.exe` 通过 VS 18 vcvars 可编译。

## 未覆盖（故意）

- 真机拖分栏手感（需要桌面 App）
- minidump 二进制（异常过滤器里分配不安全；只写 json 现场）
- IM 出进程 / 侧栏浏览器独立窗

要验收：重启 App，拖侧栏过最宽应发黏、松手回弹到上限；Win 侧栏无毛玻璃；系统「减少动态」时壁纸视频停。
