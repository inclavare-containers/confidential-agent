# MCP 网关与 CMaaS

`cai-gateway` 运行在每个 Guest 内，接管本服务的 mesh 端口，为同一状态目录内的服务调用提供：**调用方服务身份校验**，以及（对 `mcp_ports`）**MCP 调用审计、虚拟 MCP 工具和 TEE 证据绑定**。

本文以 CMaaS 示例（位于 [`examples/cmaas`](../../examples/cmaas/README.md)）为例说明它的工作方式。CMaaS 把一个标准 MCP memory server 放进机密服务，由 gateway 和 TNG 在业务端口前负责访问控制和审计。

CMaaS 不定义新的 MCP 协议，也不把 `mcp-proxy` 当作安全代理。应用侧仍是一个监听 `127.0.0.1:8000/mcp` 的标准 MCP server；安全边界由 `cai-gateway`、TNG（RATS-TLS）、mesh 服务身份和 TDX 远程证明共同提供。

## AppSpec 端口契约

CMaaS 的服务声明如下：

```yaml
service:
  id: cmaas
  ports: [8000]
  connect: []
  mcp_ports: [8000]
  app_service: cai-cmaas-mcp-proxy.service
```

这些字段的含义是：

- `service.ports`：服务声明的业务端口集合。CMaaS 只有 `8000`，MCP endpoint 对 peer 来说仍然是 `:8000/mcp`。
- `service.connect`：允许 host `connect` 和 A2A connect 客户端通过单向 RA 验证服务端后访问的端口子集。CMaaS 明确设置为 `[]`，表示 memory API 不暴露给这些入口；普通 raw `curl` 不属于合法 connect 客户端。
- `service.mcp_ports`：声明哪些业务端口按 MCP JSON-RPC 解析。CMaaS 把 `8000` 放入 `mcp_ports`，因此 gateway 会在该端口启用 MCP 审计、虚拟 tools 注入和 `tee_attest` 绑定逻辑。
- `service.app_service`：Guest 内实际运行 MCP HTTP adapter 的 systemd unit。daemon 用它判断应用是否 ready。

本文中的 `mesh_ports` 指 daemon 根据 `service.ports - service.connect` 派生出的 confidential-only mesh 端口集合，不是 AppSpec 中需要用户手写的新字段。对 CMaaS 而言：

```text
ports       = [8000]
connect     = []
mesh_ports  = [8000]
mcp_ports   = [8000]
```

因此 `8000` 同时满足两件事：它是只允许 mesh 内 TDX peer 访问的 confidential-only 端口，也是需要 MCP 解析和审计的端口。按照 spec 约束，`mcp_ports` 必须是 `mesh_ports = ports - connect` 的子集。

## 访问边界

CMaaS 的 `8000` 只允许 mesh 内 TDX peer 通过双向 RA 访问。

- 非 TEE host、baseline ECS、普通 `curl`、外部 A2A caller 没有合法的应用层访问路径。
- 即使网络安全组临时放通了来源 IP，普通 caller 也不能通过 TNG/RATS-TLS 的 TDX evidence 校验。
- 未通过 RA 或未携带有效 CAIG service identity 的请求不会进入 upstream MCP server，也不会产生 MCP audit 记录。

`connect: []` 是这个边界的关键配置。它把 CMaaS memory API 从 host connect 和跨组织 A2A 入口中拿掉，只保留 mesh confidential-only 路径。

## 运行组件

CMaaS e2e 使用两个 TDX service，每个 service 内部由以下运行组件组成：

- `cmaas`：运行标准 MCP memory server。`install-cmaas.sh` 安装 `mcp-proxy@6.5.0` 和 `@modelcontextprotocol/server-memory@2026.1.26`，把 stdio memory server 转成 Streamable HTTP，监听 guest 内 `127.0.0.1:8000/mcp`。
- `cmaas-agent`：运行一个最小 health service，使它成为 active mesh peer；同时安装 `cmaas-agent-client`，从 `/etc/cai/service-directory.json` 发现 `cmaas` 的 mesh 本地入口，并把 MCP tools 转成 OpenAI-compatible function tools 给模型调用。
- `cai-gateway`：在 client-side 为 mesh 出站连接签发 CAIG service identity token；在 server-side 的 `mesh_ports` 上验证 CAIG frame 和 service identity token，并在 `mcp_ports` 上执行 MCP 审计和虚拟 tools 逻辑。
- TNG/RATS-TLS：在 mesh 路径上提供 TDX 远程证明和加密通道。CMaaS 的 `8000` 是 confidential-only mesh 端口，调用双方都需要通过 RA。

`mcp-proxy` 只负责协议适配：把 upstream stdio MCP server 变成 HTTP MCP endpoint。它不验证 TDX evidence，不验证 CAIG service identity，不维护审计链，也不应该被描述成 security proxy。安全语义在 `mcp-proxy` 前面的 gateway/TNG 层完成。

## Mesh 请求路径

一次 `cmaas-agent` 调用 `cmaas:8000/mcp` 的路径如下：

1. `cmaas-agent-client` 读取 `/etc/cai/service-directory.json`，找到 `cmaas` 对应的本地端口，并连接 `127.0.0.1:<service-directory-port>/mcp`。这个端口是 gateway/TNG 为 mesh peer 暴露的本地入口，不是 upstream memory server 直接对外监听的端口。
2. client-side `cai-gateway` 根据自身 `gateway_identity.seed` 签发短 TTL 的 CAIG service identity token。token 中包含 issuer service、audience service/port、mesh generation、`iat`、`exp` 和唯一 `jti`。
3. client-side gateway 把 token 放入 `CAIG` frame，随后把连接交给隐藏 TNG client route。
4. TNG 建立 RATS-TLS 通道，并验证对端 TDX evidence。因为 `8000` 属于 `mesh_ports`，这是 mesh peer 间的双向 RA 路径。
5. `cmaas` 侧 TNG 只把通过 RA 的流量转给 server-side `cai-gateway`。
6. server-side gateway 在 `mesh_ports` 的 server route 上读取 `CAIG` frame，使用 mesh bundle 中的 trusted service public key 验证 token 签名、audience、过期时间和 `jti` 重放状态。token 也携带 `mesh_generation`，用于把 caller identity 放在对应 mesh bundle 上下文中解释。
7. service identity 通过后，gateway 根据 route protocol 处理流量。`8000` 同时在 `mcp_ports` 中，因此 gateway 按 MCP JSON-RPC 解析请求。
8. 普通 MCP 请求被转发到 upstream memory server；gateway 注入的虚拟 MCP tools 在 gateway 本地处理。
9. gateway 追加审计记录后才把响应返回给 caller。审计追加失败时，gateway 会扣留 upstream response，避免出现“业务成功但无审计记录”的状态。

这里的 caller identity 不是 HTTP header，也不是 MCP 参数里的自报字段。它来自 CAIG service identity token，并且只有 mesh bundle 中登记过公钥的服务可以通过 server-side gateway 校验。

## MCP Gateway 行为

`mcp_ports: [8000]` 只改变 gateway 对该端口流量的理解，不改变应用监听方式。upstream MCP server 仍然监听 `127.0.0.1:8000/mcp`，mesh peer 仍然访问服务端口 `8000`。

gateway 对 MCP 流量做三类处理：

1. 解析 MCP JSON-RPC 请求，记录 `method`、`tool_name`、参数 hash、响应 result hash 和 caller service identity。
2. 拦截 `tools/list` 响应，把 gateway 虚拟 tools 注入到 upstream memory tools 旁边。
3. 对 `tools/call` 中的虚拟 tool 在 gateway 本地返回结果，不转发给 upstream memory server。

CMaaS 暴露两类 MCP tools。

普通 memory MCP tools 由 upstream `@modelcontextprotocol/server-memory` 执行，例如：

- `create_entities`：写入 `/var/lib/mcp-memory/memory.jsonl`。
- `open_nodes`：读取 memory entity 和 observation。

gateway 虚拟 MCP tools 由 `cai-gateway` 本地执行：

- `audit_status`：返回当前审计链状态，包括记录数、最新 chain hash 和方法分布。
- `audit_verify`：重新读取 audit JSONL，校验每条记录的 hash 链，返回 `valid`、`total_records`、`latest_hash` 和错误列表。
- `tee_attest`：读取当前 audit chain digest，结合调用方传入的 `nonce` 构造 runtime binding，再调用 `attestation-challenge-client get-evidence` 获取 TDX evidence。返回值包括 `runtime_data_sha256`、`runtime_binding`、`evidence_sha256` 和 evidence JSON。

这些虚拟 tools 通过标准 MCP `tools/list` 和 `tools/call` 暴露。agent 不需要调用 gateway 私有 API，也不需要知道 TDX quote 获取细节。

## 审计链

CMaaS 的 MCP 审计文件位于：

```text
/var/lib/cai-gateway/audit-8000.jsonl
```

每个 MCP JSON-RPC 请求追加一条记录。主要字段包括：

- `caller_service_id`：server-side gateway 根据 CAIG service identity token 验证出的调用方服务，例如 `cmaas-agent`。
- `caller_jti`：service identity token 的唯一 ID，用于重放检测和单次调用追踪。
- `method`：MCP JSON-RPC 方法，例如 `initialize`、`tools/list`、`tools/call`。
- `tool_name`：当 `method=tools/call` 时记录工具名，例如 `create_entities`、`open_nodes`、`audit_status`、`audit_verify`、`tee_attest`。
- `params_hash`：请求 `params` 的 canonical JSON SHA-256。
- `result_hash`：响应 `result` 的 canonical JSON SHA-256。
- `http_status`：gateway 返回给 caller 的 HTTP 状态。
- `prev_hash` / `chain_hash`：审计链完整性字段。

审计日志不保存 memory 内容明文，也不复制用户 prompt 或 observation 明文。它记录的是“哪个可信 service 在什么时候调用了哪个 MCP method/tool，以及当时请求参数和响应结果是否能与记录匹配”。

## `tee_attest` 与 audit digest 绑定

`tee_attest` 是 gateway 虚拟 MCP tool，不是 upstream memory server 的工具。调用方通过标准 MCP `tools/call` 传入 `nonce` 后，gateway 会构造 runtime binding：

```json
{
  "schema": "confidential-agent/gateway-tee-attest-binding/v1",
  "nonce": "<caller nonce>",
  "audit_chain_digest": "<latest audit chain hash>",
  "audit_total_records": 123,
  "upstream_port": 8000,
  "timestamp": 1760000000
}
```

gateway 对该 binding 做 canonical JSON hash，得到 `runtime_data_sha256`，再把这个 hash 作为 runtime data 请求 TDX evidence。这样 agent 可以把三件事关联起来：

1. evidence 来自正在运行的 TDX guest。
2. runtime data 承诺了当前 gateway audit chain digest。
3. audit chain digest 对应 `/var/lib/cai-gateway/audit-8000.jsonl` 的当前状态。

这不是把完整审计日志塞进 quote，而是把当前审计链摘要绑定到 TEE evidence。远端 agent 拿到 evidence、runtime binding 和 audit verification 结果后，可以判断“这个审计链状态属于这个 TEE 实例”。

## 两条审计验证入口

CMaaS 明确区分 operator 视角和 agent 视角。

operator CLI 入口在 `cmaas` guest 内执行：

```bash
cai-gateway audit-verify --audit /var/lib/cai-gateway/audit-8000.jsonl
```

它直接读取审计 JSONL，验证 hash 链是否被修改、删除或重排。这个入口适合运维、调试、e2e 验收和离线检查。

agent 虚拟 MCP tools 入口通过 mesh MCP 调用执行：

```text
tools/call audit_status
tools/call audit_verify
tools/call tee_attest
```

这个入口适合远端 mesh agent 在不登录 `cmaas` guest、不调用 gateway 私有命令的情况下，通过标准 MCP 协议获取审计状态、审计完整性结果和绑定 audit digest 的 TDX evidence。

两条入口校验的是同一条 audit chain。CLI 给 operator 本地可观测性；虚拟 MCP tools 给 agent workflow 可编排的证明能力。

## E2E 验收点

`tools/e2e/run.sh cmaas` 覆盖以下场景：

1. `cmaas-agent` 通过百炼模型自然语言驱动 MCP function calling，实际调用 `create_entities`、`open_nodes`、`audit_status`、`audit_verify` 和 `tee_attest`。
2. memory roundtrip 返回刚写入的 marker 和 observation，证明 upstream memory tools 仍由标准 MCP server 执行。
3. `tools/list` 中能看到 upstream memory tools，以及 gateway 注入的 `tee_attest`、`audit_status`、`audit_verify`。
4. agent 通过标准 MCP `tools/call audit_status` 获取审计链状态。
5. agent 通过标准 MCP `tools/call audit_verify` 获取 `valid=true` 的审计链校验结果。
6. agent 通过标准 MCP `tools/call tee_attest` 获取带 `audit_chain_digest` runtime binding 的 TDX evidence。
7. operator 在 `cmaas` guest 内执行 `cai-gateway audit-verify --audit /var/lib/cai-gateway/audit-8000.jsonl`，确认审计链完整，并能看到 `create_entities` 和 `open_nodes` 等 MCP tool 记录。
8. 非 TEE baseline ECS 对 `https://<cmaas-ip>:8000/mcp` 的 raw curl 在 RATS-TLS/RA 阶段失败，失败请求不会新增 gateway audit 行。
9. snapshot-derived disk 在 TEE 外 raw block 搜索不到 runtime marker，证明运行时写入的 memory 内容没有以明文出现在外部 snapshot 搜索结果中。

## 边界

- CMaaS v1 不把 memory API 暴露到 `connect` 或 A2A AgentCard；它只允许 active mesh TDX peer 访问。
- `mcp-proxy` 不是安全代理。它只是 stdio/HTTP MCP adapter，不能替代 TNG、gateway service identity、MCP audit 或 TEE attestation。
- gateway v1 做 MCP method/tool 级审计，不实现 per-entity ACL、namespace 隔离、撤销列表或 memory poisoning 恢复。
- audit JSONL 是哈希链，不是外部透明日志。当前 e2e 通过 guest 内 operator CLI 和 agent 虚拟 MCP tools 验证它；后续可以再把 operator 远程 API 接到 agentd/TNG 通道。

## 相关文档

- [AppSpec 参考 · 三类端口](../reference/appspec.md#三类端口)
- [架构设计 · Mesh](architecture.md#6-mesh同状态目录内的可信寻址)
- [`examples/cmaas`](../../examples/cmaas/README.md)
