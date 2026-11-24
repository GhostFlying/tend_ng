# 独立 tsnet 进程

阶段目标是把已验证的 tsnet 接入作为独立可构建入口。一个进程持有节点状态目录，可同时向几个本机 Tend 地址转发及提供本机出站入口。HTTP、Runner 身份、预共享 token 与升级协议仍由 Tend 处理。

入口接受可重复的 `--inbound <tailnet-listen>=<local-target>` 和 `--outbound <local-listen>=<tailnet-target>`。进程使用同一 tsnet 节点承载各入口；终止时关闭监听与活动连接。独立测试用隔离 Tailscale 控制平面，验证两个真实节点之间的 HTTP 转发、鉴权头原样到达和重启后节点身份延续。

## 构建与运行

在 devcontainer 中从模块目录构建和验证。

```sh
cd transport/tsnet
go build -o tend-tsnet .
go test ./...
```

Server 侧可以使用 `./tend-tsnet --hostname tend-server --state-dir ./node --inbound ':8832=127.0.0.1:8832'`。Runner 侧可以使用 `--outbound 127.0.0.1:9882=<server-tailnet-address>:8832`，两代 Runner 都将 `TEND_URL` 指向这个本机入口。需要向两代 Runner 读取原 Run 时，可为各自的监听地址分别指定入站转发。首次入网从运行环境提供 `TS_AUTHKEY` 或完成 tsnet 的交互式认证。节点状态目录由通信进程持有。
