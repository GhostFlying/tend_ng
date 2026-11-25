# 调查交付与角色指引

## 目标与边界

派发保留用户原目标、授权和本轮停点。只读调查或待确认建议以一份主报告交付，有实际用户决定时沿现有 Attention 等待。直接授权的实现按授权完成。独立审阅聚焦复杂实现的实际候选，调查仅在用户明确要求或新证据可能改变关键结论时做定向复核。

仅调整三条实际注入的角色指引和对应契约。不新增硬流程、字数卡口或 category，不改变状态机、协议、存储或调度，不操作生产 Issue 来验证，不部署。

## 注入与验证

Router 的 `ROUTER_INSTRUCTIONS` 由 Server 传入 `Client::start_thread`，同时递增 `TOOL_REVISION`。Runner 从工具 catalogue 的 `include_str!` 载入 Issue Agent 和 Work Agent 指引，由执行入口传入同一客户端。三者都进入 App Server 的 `thread/start.developerInstructions`。

在现有 App Server fixture 的接收边界核对只读调查、等待决定、直接授权实现与复杂实现审阅的指引，运行定向 Chat 和协调场景及锁定构建、格式和编译检查。fixture 不验证真实模型的遵循情况。推送 `main` 后核对同一完整提交 SHA 的 Main CI、官方归档、下载校验值及归档内 `REVISION`。
