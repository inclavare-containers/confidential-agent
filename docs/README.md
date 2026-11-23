# 文档导航

文档按读者分组。不确定从哪里开始时，先读 [核心概念与术语](concepts.md)。

## 使用者：部署和使用机密 Agent

| 文档 | 适合场景 |
|---|---|
| [核心概念与术语](concepts.md) | 了解部署机、实例、AppSpec、远程证明、challenge/Trustee、connect/mesh 等基本概念 |
| [安装](install.md) | 选择全功能 RPM 或部署 ZIP，在部署机安装 CLI，配置 GitHub 代理 |
| [一键部署](guides/one-click.md) | 用 `one-click/install.sh` 部署 8 种 Agent 中的任意一种（官方预构建 CPU 镜像） |
| [OpenClaw 最佳实践](guides/openclaw-best-practice/README.md) | OpenClaw + 百炼 API（CPU）与 OpenClaw + 本地 vLLM（GPU TEE）两个场景的端到端教程 |
| [安全大模型推理](guides/secure-llm-inference.md) | 在 gn8v-tee 上部署加密模型 + vLLM 推理服务 |
| [手动部署](guides/manual-deployment.md) | 不用一键脚本，逐步执行镜像准备、实例初始化、部署、连接和清理 |
| [运维手册](guides/operations.md) | 查看状态与证明报告、建立隧道、SSH、网络授权、镜像与实例清理、升级、排错 |

## 进阶：多方协作与密钥托管

| 文档 | 适合场景 |
|---|---|
| [Trustee 模式](guides/trustee.md) | 用独立的 Trustee/KBS 托管机密，实例重启无需部署机在线 |
| [A2A 跨组织协作](guides/a2a.md) | 不同管理域的 Agent 通过 AgentCard 发现并经远程证明互相调用 |

## 应用开发者：把自己的 Agent 放进 TEE

| 文档 | 适合场景 |
|---|---|
| [打包自定义 Agent](guides/custom-agent.md) | 从零编写 AppSpec、安装脚本和运行时资源，构建并部署 |
| [AppSpec 参考](reference/appspec.md) | `confidential-agent/v1` 全部字段、默认值和校验规则 |
| [CLI 参考](reference/cli.md) | 全部子命令、常用参数和环境变量速查 |
| [示例目录](../examples/) | allinone、openclaw、hermes-agent、codex、claude-code、mcp、cmaas、vLLM 等可运行示例 |

## 项目贡献者

| 文档 | 内容 |
|---|---|
| [架构设计](design/architecture.md) | crate 划分、核心数据结构、build/deploy 时序、Guest 启动链、mesh、端口约定 |
| [MCP 网关与 CMaaS](design/mcp-gateway.md) | `cai-gateway` 的服务身份、MCP 审计链、虚拟工具与 TEE 证据绑定 |
| [开发者指南](development/README.md) | 源码构建、单元测试、端到端测试、代码导航、文档约定 |
| [打包与发布](development/release.md) | RPM / 部署 ZIP 构建、官方预构建镜像发布、版本发布流程 |
| [E2E 测试说明](../tools/e2e/README.md) | 云端端到端测试用例、环境变量与验收矩阵 |
| [版本说明](releases/) | 各版本 Release Notes |

## 内置文档

CLI 自带简要说明，适合离线查阅：

```bash
confidential-agent docs overview      # 也可用 workflow / appspec / ops
confidential-agent spec schema        # AppSpec 字段速查（--format json 输出机器可读格式）
confidential-agent <子命令> --help
```
