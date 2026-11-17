# OpenClaw 部署：百炼 API 与本地 vLLM

CA 把应用镜像构建、机密实例部署、远程证明和密钥释放连接起来。部署机持有云凭证，guest 经证明后取得应用资源；客户端通过 CA 保护的连接访问应用。

![安全架构](images/01-security-architecture.svg)

## 安装与凭证

部署机使用 Alibaba Cloud Linux 3 x86_64，云账号需有相应 TDX 或 GPU TEE 实例权限。

```bash
curl -fSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/master/one-click/install.sh -o /tmp/install-cagent.sh
sh /tmp/install-cagent.sh
```

安装入口只安装配套 RPM。部署参数、交互、状态等待和清理由 CLI 负责。云凭证从环境变量或 `~/.aliyun/config.json` 读取；缺失时 `up` 隐藏输入并保存到该文件。模型 API key 使用交互输入或 `DASHSCOPE_API_KEY`，不需要写入命令行。

## OpenClaw 与百炼 API

```bash
cagent up openclaw --output-dir ./projects
```

如需先编辑模板：

```bash
cagent init openclaw --output-dir ./projects
# 编辑 projects/openclaw/openclaw.yaml 和应用配置后：
cagent up --spec ./projects/openclaw/openclaw.yaml
```

`--model` 选择模型；`--dashscope-base-url` 指定兼容 API 地址。钉钉选项通过 `init`/`up --enable-dingtalk` 设置，凭证也可用 `DINGTALK_BOT_CLIENT_ID`、`DINGTALK_BOT_CLIENT_SECRET` 提供。

## 免构建部署与其他 Harness

从发布者取得签名 release 和独立认可的公钥，运行：

```bash
cagent up allinone --stage deploy --service-id my-agent --harness openclaw \
  --image-release ./image-release.json --trusted-key ./publisher.pem \
  --output-dir ./projects
```

同一镜像支持 OpenClaw、Hermes、Codex、Claude Code、QoderCLI、OpenCode、Pi 和 DSH。每个消费者独立设置服务 ID、Harness、模型和资源；部署不需要生产端 state 或 QCOW2。终端 Harness 使用 `cagent ssh SERVICE`，gateway 模式使用受保护连接。具体模式见 [all-in-one 示例](../../examples/allinone/README.md)。

## OpenClaw 与本地 vLLM

```bash
cagent up openclaw-vllm --output-dir ./gpu-projects
```

该模板构建 NVIDIA CC 驱动和 vLLM 环境，并让 OpenClaw 使用实例内模型服务。需要独立的加密 OSS 模型服务时，使用 [vLLM inference 流程](../secure-llm-inference-one-click.md)。共享 GPU 安装逻辑位于 `common/gpu/`。

## 网络与连接

默认放行检测到的部署机出口 `/32`。`--deployer-cidr` 可明确指定该值，`--allowed-cidr` 可重复或使用逗号分隔，添加其他操作员网段。部署机 CIDR 保存到部署记录，之后的 `peering apply` 无需原环境变量。

`up` 使用已有部署状态：首次构建部署、继续已确认基础设施的资源交付，或连接现有 active 实例。基础设施操作失败时返回真实错误；不要把重新初始化项目当作恢复操作。具体恢复要求见 [AppSpec](../spec.md)。

`up` 最后调用 `connect start`，连接元数据和日志保存在 `<state-dir>/services/<service>/connect-ready.json` 及相邻 `.log` 文件。使用输出里的实际本地端口访问 OpenClaw。模型调用是否成功还需通过应用验证。

```bash
cagent status --live
cagent tui
cagent connect start --service openclaw --ready-json ./connect-ready.json
cagent connect stop --ready-json ./connect-ready.json
```

宿主机 OpenClaw 客户端按需单独安装。Web、TUI 或桌面客户端使用实际本地端口及项目中保存的 gateway token；不要将 token 复制到公共日志。

## 销毁

```bash
cagent cleanup openclaw
cagent cleanup --all --dry-run
cagent cleanup --all
```

`cleanup` 使用状态中真实的服务 ID，包括非默认 Harness。它停止 `up` 创建的连接并调用正常销毁流程，保留发布镜像和本地记录；手动 `connect start` 创建的其他连接用其 ready 文件单独停止。

## PEP 与证明

OpenClaw 的 PEP 工具调用拦截是可选 PoC，可在独立 OpenClaw 模板中用 `--disable-pep` 关闭；它不是覆盖全部 Harness 的通用安全边界。其他 Harness 仍使用 `cai-pep attest` 和共享证明 Skill，不安装工具执行 Hook。

`sample` 使用本地或已认证 release 的实测参考值；`rekor` 使用公开参考元数据。当前 challenge 注册 Rekor 参考值失败后会尝试本地实测值。证明状态、资源释放与应用是否可用是不同观测，分别查看 `status --live`、证明报告和应用响应。
