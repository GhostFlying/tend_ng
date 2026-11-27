# 普通讨论回合进展

## 目标与边界

普通 Discussion 在原用户消息下方展示当前回合已经完成的 Codex 自然语言进展。运行中只显示最新一行并裁剪。回合结算后默认收起，展开可按原顺序阅读。没有进展则不占位，正式回复照常自动出现。沿用消息页轮询，不加 SSE。不改变主 Chat、Issue、讨论转交和转交预览。

## 实现

- Codex 适配器仅从已核实 thread/turn 的 `item/completed` 接收完整 `agentMessage`，由普通讨论处理器再校验 `phase: commentary`、有效 item ID 和自然语言正文。忽略开始事件、增量、工具、推理、无标记消息及明显的结构化草稿。进展采集异常不改变正式回复的结算。
- 用独立 SQLite 投影保存已接受的正文，按原生尝试和 item ID 去重，首次写入获胜。只读消息页按讨论和已取出的页范围批量读，由输入消息身份关联。尝试重放不增加条目，重试保留已见进展，按 attempt 顺序和同一 attempt 的观察顺序展示。成功的终态原生历史只有在 item 自带有效 `completedAtMs` 时才补回未及时落盘的进展。Codex 的旧版兼容历史可能重建无完成时间、使用合成 ID 的 item，不用它猜测进展。现有正式消息、讨论历史工具和转交材料不混入进展。不回填已结算旧回合。
- 前端仅在普通 Discussion 用户消息下渲染进展，正文作为 React 纯文本。运行态和结算态用同一个位置，后者提供原生可折叠控件。保持 390px 布局紧凑。

## 验证与交付

- 覆盖原身份去重、进展过滤和作用域边界。复跑普通讨论、快转与接续相关定向测试。
- 真实 Server 与固定 App Server fixture 相连，用可控闸门把正式回复挡在浏览器断言之后。Chromium 验证进展替换而非堆叠、裁剪、刷新、结算收起及展开、正常回复、空轮、错误类型与异回合事件过滤、重启恢复与重复回放、纯文本安全渲染、桌面和 390px 手机布局。
- 依仓库规定直接合入 `origin/main`，核对完整远端 SHA，交付实际桌面和手机渲染图。不部署，不发生产测试讨论。

在已准备 Chromium 的开发环境复跑。截图可选，写到仓库外。

```sh
cargo build --locked
pnpm --dir web install --frozen-lockfile
pnpm --dir web build
python3 -B tests/e2e/discussion_progress.py target/debug/tend-server --screenshots /tmp/discussion-progress
```
