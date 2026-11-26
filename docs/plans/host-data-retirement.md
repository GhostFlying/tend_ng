# 同机执行数据与退役清理

目标是让同机接替后的历史 Run 和交付继续沿原身份读回并补传，生产工作区、仓库与 Codex home 继续共享。Server 记录原产出者，接替关系授予后继只读历史和补件职责。每代独立 SQLite 保留到全部领取、操作、交付和交接获得持久确认且原进程停妥。

在正常升级入口集成退役检查和可重试的旧格式迁移。按具体文件清理旧代私有状态，保留共享目录、回滚备份及仍由运行服务引用的文件。常用资源目录移除已退役实例。

验证覆盖服务端路由、App Server/home 绑定、跨代补件、响应丢失、文件不齐时保留、升级脚本重跑与权限边界，结合固定输出执行真实 Server/Runner 集成及桌面和手机页面渲染。

## 交付与验收

资源页在桌面和手机上保留正常运行、排空中和离线的实例。已退役的实例保存在 Server 的历史登记中。使用相同 fixture 打开两种资源页，渲染结果见 [Agents 桌面](images/retirement-resource-agents-1440.png)、[Agents 手机](images/retirement-resource-agents-390.png)、[主机桌面](images/retirement-resource-host-1440.png) 和 [主机手机](images/retirement-resource-host-390.png)。

同一条已登记的接替链负责历史读取与文件补回。普通和 Ark 类启动器分别沿原 thread 的绑定读取，Server 校验返回的原 Runner、Run、turn 与实际读者。旧库的单次布局迁移沿 [部署说明](../../README.md#从旧的每代私有目录迁入) 执行。每代独立库的收件、outbox 和交接回执保留到持久同步完成。
