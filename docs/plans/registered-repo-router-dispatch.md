# Router 直接派发已注册仓库的工作

## 目标与边界

每个 Router 回合都从 Server 已收到的当前 Runner profile 读取仓库 ID。排除已被接替的 Runner，去重并排序，只注入 ID，不引入别名、远端元数据、查询工具或新的分配规则。

用户明确指定已注册仓库并要求开始新的工作时，Router 使用原仓库 ID 直接派发一次，保留目标、授权和停点。Router 不预查候选 Agent 或 MR / PR，也不指定 Agent 或 Runner。Work Agent 使用自己的权限查找 MR / PR。未命中时仍按既有路由处理，重放时沿已提交的回执去重。

## 验证

- 用隔离 Server、真实 Runner 同步 API 和固定 App Server fixture 验证仓库列表、会话接续、普通答复和审阅请求的实际派发记录。核对已接替 Runner、去重排序、无候选查询、无 MR / PR 查询和只读停点。
- 运行 Server Rust 测试和既有 Chat 恢复 fixture，核对上下文与重放兼容。固定 fixture 验证执行契约，不代表真实模型选择。
