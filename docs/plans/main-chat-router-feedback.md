# 主 Chat 的派发受理与 Router 进展

## 目标和边界

Router 按工作内容判断是否需要读仓库或多步核验，优先派给 `investigation` Owner，已有匹配 Issue 时复用。不加关键词路由，不重复上线 Lynx weekly，不调用线上资源，本次交付到 `origin/main`，不部署。

- 主 Chat 的新派发以及复用 Issue 的入队补充，在各自已有事务内写一条回复原消息、链接目标 Issue 的受理消息。写入只表示 Tend 已持久受理，不表示 Runner 已领取。只记待办、查原回执或失败不发布本次派发回执。
- Router 自行处理耗时工作时，提示它先用 `tend_publish_progress` 发布一句具体计划。工具只接受主 Chat 的真实当前用户输入，按原 Router 输入保存第一条进展和消息回执。回合重放或工具重试返回同一消息，不发布新内容。普通回答与后台事件不加进展，不发布原生增量或思考。
- 沿用 Chat 完整消息、回复关系、Issue 短编号和既有 Web 轮询，无需新 API 或 SSE。回执可见时只在回执上显示同一 Issue 链接，原输入仍保留关联。区分即时反馈与最终回复，主 Chat 历史仍保留消息。

## 验证

使用脱敏 Lynx weekly 比较请求和可控 App Server fixture。让隔离 Server 完成派发后暂停 Router，Runner 仍未领取，验证 CLI、实际浏览器与 Issue 链接。对自行处理分支，让 fixture 进入一个未完成的本地工具步骤后观察进展和仍未结束的回合。另测同一操作重试、回合中断后重放、拒绝的派发、已有关联 Issue 的复用，以及普通直接回答。

覆盖 Rust 定向测试、现有 Chat/路由回归和实际 Web 渲染。最终证据截取自隔离 fixture，证明发布契约与时序，不证明模型对相似自然语言必选某条路径。

构建 Server、CLI 与 Web 后，可用 `python3 -B tests/e2e/router_feedback.py target/debug/tend-server target/debug/tend` 复跑。附加 `--screenshots <输出目录>` 时，同一 fixture 会同时检查桌面和手机宽度的真实页面，点击 Issue 链接，并在释放两个执行阻塞点之前保存截图。

## 审阅后补充验证

一条输入派发两个 Issue 后，分别验证引用第二条受理回执，以及引用后续补充的受理回执。Router 的明确目标均只包含该回执直接链接的 Issue，不混入原输入还关联的兄弟项。

丢失原生线程后，用不带引用的新用户输入启动 Router。独立注入近期已受理工作的结构化事实，保留原消息、受理消息、Issue ID 和编号。只含回执的派发也可恢复，临时进展不作为历史回答恢复。保持现有的派发操作身份和回放查证方式，允许同一用户输入的不同目标分别派发。
