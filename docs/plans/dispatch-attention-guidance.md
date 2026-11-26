# 派发与 Attention 角色指引修正

## 目标与边界

- Router 派发保留用户具名目标、明确限制和停点。面向具体仓库的依赖保全步骤交给 Work Agent，仅在实际效果会实质扩大目标或授权时带证据确认。直接派发、已注册仓库 ID 和重放规则保持不变，递增 Router 指引版本以启用新线程。
- Issue Agent 有权决定时答复原子 Attention，需要用户决定时用原 ID 升级并保持原问题未答。新 Attention 只用于 Issue Agent 自己的新问题。
- Work Agent 依据输入中的直接证据区分用户亲答和其他答复，证据不足时仅陈述决定。
- 只修改角色指引、相应契约和验证，不改状态机、协议、存储、调度、工具集合或投递逻辑。验证使用隔离合成数据，不操作真实 MR / PR 或线上 Issue，不部署。

## 验证

- 断言 App Server 实际收到的 Router、Issue Agent 和 Work Agent 指引。定向 fixture 覆盖注册仓库的直接派发与重放、原子 Attention 升级与属权内答复、用户亲答与上级答复的输入区别。
- 运行格式、锁定构建、定向 Rust 测试与注册仓库派发、协调、Attention 及 Issue Attention 端到端测试。
- 用独立已认证 Codex home、临时 Server / Runner、SQLite 和仓库观察真实模型的派发与原 ID 升级。可行时观察无用户亲答证据的 Work Agent 报告。记录原输入、实际工具选择和持久证据，注明未覆盖的边界。
