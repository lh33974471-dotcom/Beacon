# 纯内网直连，彻底剥离服务器依赖

Status: accepted

产品只在企业内网使用，因此删除 hoptodesk 对 api.hoptodesk.com、rendezvous WebSocket、TURN 中继的全部依赖，连接路径只保留"输入 IP（或在线列表点选）→ TCP 直连被控端"。ID/中继相关的 UI 与死代码一并裁掉。

## Considered Options

- 保留服务器逻辑作为可选 fallback：拒绝。无外网环境下它只会带来启动超时、外联合规噪音和攻击面，日后若需要跨网能力再从上游重新引入。
- 自建私有 rendezvous 服务器：拒绝。内网直连不需要任何服务器，引入服务器等于引入运维负担和单点故障。

## Consequences

- 跨互联网远程能力被永久移除，重新加回成本高。
- 被控端不再向任何外部地址发起连接，便于内网安全审计。
