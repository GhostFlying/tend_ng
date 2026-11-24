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

## 保留已有 HTTPS 入口

独立进程通过 `--inbound-tls ':443=127.0.0.1:8832'` 在 tailnet 节点终止 HTTPS。证书由 tsnet 按节点域名取得和续期，解密后的 HTTP 发往本机 Tend Server。入站 HTTPS 可以和普通入站及出站转发共存，Tend Server 继续校验原始请求的 token。

沿用已有 Web 域名时，先停妥原 tsnet 实例，再用同一台机器上的原节点状态和原节点名称启动独立进程。状态目录仅有一个运行中的持有者。Tend 新建自己的业务库和工作目录。

在隔离控制平面上验证普通转发和原节点身份恢复，并用受信的测试证书验证 HTTPS 转发、主机名校验和鉴权头。切换时从另一台宿主机按旧 HTTPS 地址检查证书和 Web 响应，再核对两台 Runner 的联系及独立通信通道。
