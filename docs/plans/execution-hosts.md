# 执行主机运行明细

## 范围

按已确认的 [桌面交接设计](images/execution-hosts-design-desktop.png) 和 [手机设计](images/execution-hosts-design-mobile.png) 替换 Web 旧主机详情弹窗。按 Host 展示占用和当前世代，Runner 可独立展开，首见有工作时展开。以顶层 Issue 分组，每行显示执行中的 Task、原派发具名 Agent、该次 Run 的时长，并链接对应 Issue。用产品本身的导航和配置，不加入设计稿的演示切换。已退役不出现或计数，收尾在运行时可见，同机重叠按一份接单代容量计。离线或读不到新快照时标明不能确认当前状态。

## 事实和读写边界

- 已收到 App Server 原生回合启动确认后，Runner 在持久化 Run Started 的同一事务写入自身的 Unix 秒。补录已恢复的历史开始、旧代或没有可确认的时间时保留未知。开始时间随已有 Run 事实对账，无独立请求或表。协议版本不变，可选字段旧 Server 可忽略；同原事件带或不带时间可确认，无权改变已写入的时间或其他事实。
- 认证只读 `GET /api/v1/execution-hosts` 对应 `tend runner activity`，不扩展工作调度，不改变原 Runner CLI 列表。一次短读事务获得 roster、原持久归属和可运行明细，使用 Started Run 部分索引，根 Issue 只追溯待展示的活动行，不扫历史或原生原件。用原 assignment 的具名 Agent，不查询当前目录猜测执行者。心跳报告为零或过期的 Runner 不展示为活动明细。
- 主机容量不累计作为前驱正在收尾的配置；各代运行计数来自当前同步的 Runner slot。启用中的多个独立接单实例按各自配置计，接替链的未激活 standby 不计。轻量读取只随打开的页面定时刷新，保留展开状态，不逐行请求。

## 验证

在 devcontainer 使用固定 App Server fixture 和临时真实 Server / Runner。核对真实 Started、停妥或 wait、旧代没有起始时间、组树、收尾与退役、离线、两端可选字段兼容和查询索引。桌面和窄屏核对展开、聚合、空态、导航、无溢出及可访问交互，并提供最终渲染。

## 实际渲染

[桌面 1440 px](images/execution-hosts-render-desktop.png) · [手机 390 px](images/execution-hosts-render-mobile.png)。实际 Web 使用真实 Server 和脱敏固定输入。示例包含同 Host 新旧代各自的 Run、未知启动时刻及空闲机器，不表示现网工作。
