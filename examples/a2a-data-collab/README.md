# A2A 数据协作

`analyst.yaml` 和 `data-owner.yaml` 分别部署分析 Agent 与数据持有者。应用使用共享 Node 安装库；示例数据位于 `files/private-risk-data.jsonl`。

先创建 `secrets/`，把对应的 `*.example.json` 复制为 `secrets/analyst.json`、`secrets/data-owner.json`，填写自己的模型凭证。密钥不应提交到仓库。生成构建签名密钥后，可在两个独立 state directory 中构建和部署：

```bash
cagent key generate-cosign --output-key-prefix ./cosign
cagent --state-dir .state/analyst up --spec analyst.yaml
cagent --state-dir .state/owner up --spec data-owner.yaml
```

部署完成后，在 data-owner 的 peerings 中放行 analyst 的 AgentCard/connect 来源，再在 analyst 中执行 `a2a add`，使用 data-owner 的 AgentCard URL 和 `--alias data-owner --service analyst-agent`。完整双向流程及签名示例见 [A2A 文档](../../docs/a2a.md)。
