# 示例与共享镜像

| 入口 | 用途 |
|---|---|
| `allinone/instance.yaml` | 从发布者提供的共享或云市场镜像直接部署，实例独立选择 Harness 和资源 |
| `allinone/build.yaml` | 镜像发布者构建共同运行环境，随后 publish/export |
| `openclaw/`、`hermes-agent/`、`codex/`、`claude-code/` | 开发者定制单个 Harness 的构建模板 |
| `openclaw-vllm/`、`vllm-inference/` | NVIDIA CC 与 vLLM 示例；后者挂载加密 OSS 模型 |
| `mcp/`、`cmaas/`、`a2a-data-collab/` | MCP、管理服务和跨 Agent 协作示例 |

建议先用 `cagent init TARGET` 生成独立目录，然后依次运行 `build`、`deploy` 和 `connect start`。修改生成的 AppSpec 和安装脚本即可定制镜像。直接使用仓库 YAML 时，相对路径以 YAML 所在目录为基准；`/usr/bin/cai-pep` 是 RPM 路径，源码开发可通过 `CA_PEP_BIN` 配合 `init` 选择自己的构建产物。模型配置和密钥放在生成项目或各示例的 `secrets/` 目录中。

`common/` 保存内部共享资产：Node 安装库、CLI Harness 安装器、NVIDIA CC/vLLM 安装库及本地证明 Skill。CA 构建时会提供公共 guest 安装库，RPM 同时携带这些模板。`common/` 不是独立应用示例。

`cai-pep attest` 为各 Harness 提供本地证明工具。OpenClaw 的执行拦截插件是可选 PoC，只覆盖该示例中的工具路径，不宣称为其他 Harness 提供通用强制访问控制。
