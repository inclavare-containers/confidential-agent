# CMaaS MCP Gateway / E2E 示例

这个示例是 MCP gateway 端到端能力的主入口：标准 MCP memory server 仍然监听 guest 内 `127.0.0.1:8000/mcp`，Confidential Agent 在端口前接管 mesh 访问、CAIG service identity、MCP 审计、虚拟 MCP tools 和 TEE evidence 绑定。

服务声明固定为：

```yaml
service:
  id: cmaas
  ports: [8000]
  connect: []
  mcp_ports: [8000]
  app_service: cai-cmaas-mcp-proxy.service
```

`connect: []` 表示 `8000` 不对 host connect、A2A connect 或普通非 TEE caller 暴露。`mesh_ports` 由 `ports - connect` 派生，因此 CMaaS 的 `mesh_ports` 就是 `[8000]`。只有 mesh 内 TDX peer 通过 RATS-TLS 双向远程证明后，才能访问这个端口。

`mcp_ports: [8000]` 表示 gateway 会把 `8000` 上的业务流量按 MCP JSON-RPC 解析，额外启用审计、虚拟 tools 注入和 `tee_attest` audit digest 绑定。它不改变 upstream MCP server 的监听端口，也不要求应用实现新的 MCP 协议。

## 组件

- `cmaas`：安装 `mcp-proxy@6.5.0` 和 `@modelcontextprotocol/server-memory@2026.1.26`，把标准 stdio memory MCP server 转成 Streamable HTTP，监听 guest 内 `127.0.0.1:8000/mcp`。
- `mcp-proxy`：只是 stdio/HTTP adapter，不是安全 proxy。它不验证 TDX evidence，不验证 CAIG service identity，也不维护审计链。
- `cai-gateway`：在 `mesh_ports` 上验证 CAIG frame 和 service identity token；在 `mcp_ports` 上解析 MCP、写审计链、注入 `audit_status`、`audit_verify`、`tee_attest`。
- `cmaas-agent`：作为 mesh TDX peer 运行 `cmaas-agent-client`，从 `/etc/cai/service-directory.json` 发现 `cmaas`，并通过自然语言任务驱动模型调用 MCP tools。

## 安全路径

一次 agent 调用会经过以下路径：

1. `cmaas-agent-client` 从本机 service directory 找到 `cmaas` 对应的本地端口并发起 MCP 请求。
2. client-side gateway 签发短 TTL CAIG service identity token，并把 token 写入 `CAIG` frame。
3. TNG/RATS-TLS 建立 mesh 通道，双方验证 TDX evidence。
4. `cmaas` 侧 gateway 在 `mesh_ports=[8000]` 的 server route 上验证 caller service identity、audience、过期时间和 `jti` 重放状态。
5. 通过身份校验后，gateway 对 `mcp_ports=[8000]` 的请求执行 MCP 审计和虚拟 tool 逻辑。
6. 普通 memory tools 转发给 upstream server；`audit_status`、`audit_verify`、`tee_attest` 在 gateway 本地处理。

普通 ECS 或 raw `curl` 即使能到达网络地址，也不能通过 TDX RA 和 CAIG service identity 校验，请求不会进入 memory server，也不会新增 MCP audit 行。

## 审计与证明

MCP 审计文件位于：

```text
/var/lib/cai-gateway/audit-8000.jsonl
```

每条记录包含 caller service、MCP method、tool name、请求参数 hash、响应 result hash、HTTP 状态、前序 hash 和当前 chain hash。日志不保存 memory 内容明文。

operator 可以在 `cmaas` guest 内用 CLI 校验审计链：

```bash
cai-gateway audit-verify --audit /var/lib/cai-gateway/audit-8000.jsonl
```

agent 可以通过标准 MCP 虚拟 tools 做同类检查和证明：

- `audit_status`：返回审计链记录数、最新 chain hash 和方法分布。
- `audit_verify`：返回审计链完整性校验结果，例如 `valid=true`。
- `tee_attest`：把当前 audit chain digest 和调用方 nonce 放进 runtime binding，并返回绑定该 digest 的 TDX evidence。

CLI 入口用于 operator 本地验证；虚拟 MCP tools 用于 mesh agent workflow。两者面向同一条 audit chain。

## 运行 E2E

```bash
tools/e2e/run.sh cmaas
```

（如果当前 shell 设置了 HTTP 代理，而代理无法访问阿里云 OpenAPI 和软件源，请先 `unset` 相关代理变量再运行。）

e2e 会执行以下验收：

1. `cmaas-agent-client` 通过自然语言任务让模型调用 `create_entities` 写入 memory，再调用 `open_nodes` 读回 memory。
2. 同一次 agent 流程继续调用 `audit_status`、`audit_verify` 和 `tee_attest`，确认审计链完整并返回绑定 audit digest 的 TDX evidence。
3. `tools/list` 中同时出现 upstream memory tools 和 gateway 虚拟 tools。
4. guest 内使用 `cai-gateway audit-verify` 校验审计 JSONL，确认记录中包含 `create_entities` 和 `open_nodes`。
5. 普通非 TEE baseline ECS 无法通过 RATS-TLS/RA 访问 `cmaas:8000/mcp`，且失败请求不会新增审计行。
6. snapshot-derived disk 在 TEE 外 raw block 搜索不到运行时写入的 marker。

`agent-config.json` 是示例占位配置；e2e 会渲染真实百炼配置到 `/etc/cai/cmaas-agent.json`，不会把 API key 写进日志。
