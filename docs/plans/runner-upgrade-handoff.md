# Runner 常规升级默认同机交接

## 目标与边界

- 在 Linux main-build 中提供可重试的本机升级入口。由正在运行的前驱配置派生 standby 后继，保留 Host、workspace、repo、Agent 和启动环境，独立分配 Runner 身份及 SQLite 库。
- 复用 replace、同步、drain 和 retire。只等待实际 Run 自然停妥，不发 stop、不杀前驱、不复制数据库，也不为验证向实际部署派发探针任务。
- Server 保存接替时前驱的 slot 预算。初始保守地保留全部 slot，直到前驱确认已封住新 Run 准入，再按仍占用的本地活跃 slot 逐步释放给后继。后继在实际 Run 准入处执行 Server 发放的上限。
- 保留 v1/v2 的兼容窗口。不能报告安全余量的前驱按满额处理至 drain，不能执行动态上限的后继等前驱 drain。保留原 Task、待处理输入、workspace、旧 Run 身份及缺少可验证绑定时的新原生 session 语义。后续同机 native 续接见[独立计划](runner-native-thread-handoff.md)。
- 仅交付产品代码、文档和隔离验收；不触碰生产配置、进程或数据库。

## 实施

1. 扩展同机接替账本及可选同步字段。Server 重启保留原预算和已经证实释放的份额；后继重新启动时沿本地库保留执行上限。保持旧 Runner 的字段缺省行为。接替事务拒绝会引入第三代或另一个来源的关系。
2. 归档入口记录目标 REVISION、原进程身份和派生配置。接替前核对独立进程及发布的配置事实；接替后读取 Server 事实恢复同一操作。自然 drain 后，只有原进程确实退出且后继可用时才 retire。
3. 补齐 README、操作契约、AGENTS 和 CI。

## 验证

- Rust 单测验证迁移、持久预算、单调释放、重复登记和协议兼容。
- 用真实临时 Server、两个 Runner、回环和 App Server fixture 验证归档脚本接替、重试、Server 重启、旧 Run 自然停妥、原 Task/输入/workspace 接续和完整 slot 恢复。
- 回归协议窗口、外部事件接续和绑定审阅接替；检查 main-build 打包、相同 SHA 的 CI 和归档。

## 独立审阅后的补充

- 文件审阅跨代时优先保留原本机快照读取。快照不在后继时，按同一 artifact ID 从 Server 已登记的不可变副本恢复，完整校验长度后原子落盘。暂时未上传或链路中断时等待原 Run 重试，不使用可变工作区文件。隔离测试覆盖已领取但未启动的首轮文件审阅和不同内容的工作区文件。
- 入口以本机 SQLite 的持久 Runner/Host 为准，核对现场配置、同一数据库的实际进程、Server 和恢复记录；身份冲突时不启动 standby、不变更接替关系。增加配置路径内容被替换的回归。
- 同库锁使用规范路径，禁止 SQLite 硬链接副本作为独立库，以两个持久进程验证符号链接不能绕过排他。
- 文档给出旧 systemd unit 使用 `Restart=on-failure` 的配置步骤。预算只绑定接替关系，未参与接替的同 Host Runner 仍独立。
- 复审补充默认 workspace 根的尾点数据库名回归。归档入口改为调用目标 Runner 的只读默认路径查询，与运行时共用 Rust 路径规则；用 `runner.` 作为前驱库验证原 Task 与 `runner.runs` 接续。
