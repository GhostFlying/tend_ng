# 顶层 Issue 直接领取与交付终点

## 目标与边界

- 一人可交付的工作从主 Chat 直接派到原顶层 Issue 的第一个 Task。Router 根据目标明确选择 implementation、investigation，或在范围小且路径明确时选择 implementation-economy。只有独立交付需要协调时才用 issue-router；未指定 category 的旧输入仍为 issue-router。
- category 随原派发持久保存，待安排 Issue 开工沿原意图。具名配置、仓库、能力、容量、领取与同 Host 接替沿现有契约。直接 Owner 使用原顶层仓库 workspace，可交付、上报、结案、提问和按需委派。独立审阅继续绑定实际作者 Task。保留原 PR/MR 发现、等待、续跑、退役和历史；不改 memory、不添加 CI 状态机、不部署。
- 三种实际注入的责任指引均保留原授权的终点。合入请求在满足仓库门禁、定向验证和核对远端 main SHA 后回报。仅在明确要求时等待合入后的 CI、出包或部署。当确实需要观察时由一位责任人登记并共享精确版本与结果，等已有子项事件时结束回合。

## 实施

1. 在 Router 派发与录入边界增加顶层 category，并经原请求保存，沿现有匹配落实角色。用 Runner 可选能力避免把新增的顶层执行契约发给不认识它的旧 Runner。
2. 直接 Owner 复用现有 Work Agent 指引与交付、审阅工具，在顶层领取身份上追加报告与协调工具。仓库挂载和分支发现沿原 workspace；Server 验证原 Task/Issue 所属关系。
3. 同步更新角色/派发/工具/生命周期契约。

## 验证

- Rust 定向覆盖旧记录默认、显式 category 开工、具名配置和 Runner 兼容；运行格式检查、Rust 构建和相关回归。
- 隔离 Server、Runner、CLI 与临时 Git 仓库通过固定 App Server 输入验证原顶层 Task、无占位子项、工作区及原 Chat 报告；分别覆盖合入即交付和显式 CI 等待、实际独立委派、同 Host 接替和原 Task 唤醒。Fixture 验证工具与指引注入和系统执行，不作为模型独立决策的证明。
- 仅使用预先认证的隔离 Codex home 通过 live_codex.py 验收真实模型。没有可用的隔离认证时记录未覆盖，不使用个人或正在运行的 Runner home。独立验证结论之外，交付前核对远端 main SHA。
