基于机密计算实例一键构建安全大语言模型推理环境
================================================

随着大语言模型技术的普及，企业对推理过程中的数据安全问题日益关注。模型权重是核心资产，推理 Prompt 与回复可能包含敏感业务数据，模型解密密钥一旦泄露将导致模型保护失效。本文介绍如何在阿里云 gn8v-tee TDX 机密计算实例上，使用一键部署脚本快速构建基于 vLLM 的安全大语言模型推理环境，通过 gocryptfs 运行时解密、Intel TDX 硬件级内存加密、远程证明和 OHTTP 端到端加密，保护模型权重、推理数据和模型密钥的安全。

方案概述
---------

本方案将 vLLM 推理服务封装在 Intel TDX（Trust Domain Extensions）可信执行环境中。模型以 gocryptfs 密文形式存储在 OSS 公共读 Bucket 中，仅在远程证明通过后将模型解密密钥注入实例，由 gocryptfs 在 TDX 内存边界内完成解密挂载，vLLM 直接读取明文模型启动推理。用户推理请求经 TNG OHTTP 加密通道进入 TDX 实例，推理 Prompt 与回复全程在 TDX 内存加密边界内流转。

本方案采用 challenge 远程证明模式：部署时，实例先通过远程证明验证运行环境可信，验证通过后才下发模型解密密钥与运行时配置，确保模型密钥不会注入到未经证明的实例中。

### 安全架构

Confidential Agent 部署中的核心数据均在 TEE（Trusted Execution Environment，可信执行环境）边界内闭环流转，以下资产受到保护：

| 保护对象   | 说明                                                                                       |
|------------|--------------------------------------------------------------------------------------------|
| 推理数据   | 用户推理 Prompt 与模型回复，可能包含个人身份、医疗、金融等敏感信息。                       |
| 模型权重   | 以 gocryptfs 密文存储于 OSS，仅在 TDX 内存内解密，云平台和宿主机无法读取明文。             |
| 模型密钥   | gocryptfs 解密密钥，仅在远程证明通过后注入实例，未证明环境无法获取。                       |

安全架构自底向上覆盖硬件、启动链、运行时、密钥和通信五个层级：

| 保护层级 | 机制                                                                                          |
|----------|-----------------------------------------------------------------------------------------------|
| 硬件层    | Intel TDX 对 Guest OS 全部内存透明加密，云平台和宿主机无法读取明文。                          |
| 启动链    | UKI（Unified Kernel Image，统一内核镜像）和 dm-verity rootfs 防篡改；镜像度量值上传 Rekor 透明日志。 |
| 运行时    | vLLM 推理服务运行在 TDX 信任域内，模型解密与推理全程在内存加密边界内完成。                    |
| 密钥管理   | 模型解密密钥与运行时配置仅在远程证明通过后注入，challenge 模式确保密钥不下发到未证明实例。     |
| 通信链路   | OHTTP 端到端加密，远程证明验证实例身份后才建立通道，推理流量全程加密传输。                |

### 部署架构

部署流程如下：

1. **凭证准备** ：在部署机上导出阿里云 AccessKey，准备模型解密密钥文件和 OSS 模型密文路径。

2. **一键部署** ：执行一键部署脚本，自动完成依赖安装、可信镜像构建、参考值上链、云资源创建、远程证明和机密资源注入。

3. **建立加密通道** ：脚本自动启动 TNG connect 隧道，建立部署机与 TDX 实例之间的 OHTTP 加密通道。

4. **访问推理服务** ：通过 SSH 端口转发部署机的 connect 端口，使用 OpenAI 兼容 API 发起推理请求，所有流量经 OHTTP 加密通道进入 TDX 实例。

前提条件
---------

* 已开通阿里云 ECS 服务，账号具备创建 TDX 机密计算实例、VPC、交换机、安全组、OSS Bucket、自定义镜像等资源的权限。

* 已准备一台 Alibaba Cloud Linux 3 ECS 实例作为部署机，可用磁盘空间不低于 80 GB。

  **重要**

  部署机仅用于执行构建和部署操作，后续一键脚本会自动创建新的 TDX 机密实例承载 vLLM 推理服务。部署机本身无需是 TDX 实例。

* 已获取阿里云访问凭证。建议使用 RAM 用户最小权限、RAM 角色或 STS 临时凭证，避免长期存储主账号 AccessKey。

* 已使用 gocryptfs 加密模型，并将密文上传到一个公共读的 OSS Bucket。模型解密密钥文件（passphrase）需妥善保管，部署时通过 `--model-key-file` 传入。

* 部署机可以访问公网，用于拉取源码、系统包、Rekor 透明日志和阿里云 OpenAPI。

### 准备加密模型

本方案的模型以 gocryptfs 密文形式存储在 OSS 中。请在部署前完成模型加密与上传。

1. 安装 gocryptfs。Alinux 3 可直接通过 yum 安装：

   ```bash
   sudo yum install gocryptfs -y
   ```

2. 创建模型解密密钥文件。建议使用随机生成的强密钥：

   ```bash
   cat << EOF > ~/model-decryption-key
   <您的随机密钥>
   EOF
   ```

3. 初始化 gocryptfs 加密目录并挂载：

   ```bash
   mkdir -p ~/mount/cipher ~/mount/plain
   cat ~/model-decryption-key | gocryptfs -init ~/mount/cipher
   cat ~/model-decryption-key | gocryptfs ~/mount/cipher ~/mount/plain
   ```

4. 将明文模型复制到挂载点，完成加密后卸载：

   ```bash
   cp -r <明文模型路径>/. ~/mount/plain
   fusermount -u ~/mount/plain
   ```

   加密后的密文存储在 `~/mount/cipher` 目录中。

5. 将密文上传到 OSS Bucket。需准备一个与即将部署的实例相同地域的 OSS Bucket，并将加密模型密文上传。因模型文件较大，推荐使用 ossbrowser 上传。Bucket 需设置为公共读，以便实例内 s3fs 以匿名方式挂载密文。

操作步骤
---------

**说明**

本文以 `ecs.gn8v-tee.4xlarge` 实例规格、地域 `cn-beijing`、可用区 `cn-beijing-l` 为默认配置。

### 步骤一：准备凭证

一键部署脚本需要通过阿里云 AccessKey 创建云资源（ECS 实例、VPC、安全组等），并通过模型解密密钥文件在远程证明通过后注入实例。在部署机上执行以下命令，将阿里云凭证导出为环境变量。请将尖括号中的占位符替换为实际值。

```bash
export ALICLOUD_ACCESS_KEY="<YOUR_ACCESS_KEY>"
export ALICLOUD_SECRET_KEY="<YOUR_SECRET_KEY>"
```

| 占位符                 | 说明                | 获取方式                                  |
|------------------------|---------------------|-------------------------------------------|
| `<YOUR_ACCESS_KEY>`    | 阿里云 AccessKey ID | 参见创建 AccessKey。                       |
| `<YOUR_SECRET_KEY>`    | 阿里云 AccessKey Secret | 与 AccessKey ID 一同生成。                |

**重要**

建议使用 RAM 用户最小权限或 STS 临时凭证，避免长期存储主账号 AccessKey。

部署时还需通过以下参数传入模型相关配置：

| 参数                | 说明                                                         |
|---------------------|--------------------------------------------------------------|
| `--model-key-file`  | 本地 gocryptfs 模型解密密钥文件路径（必须存在，部署时注入）。 |
| `--oss-bucket`      | OSS Bucket 中模型密文路径，格式为 `<bucket>:<path>`。       |
| `--oss-endpoint`    | OSS Endpoint，默认 `https://oss-cn-beijing-internal.aliyuncs.com`。 |
| `--replicas`        | vLLM 副本数，默认 `1`（单节点，OHTTP 加密 connect 隧道，无 ALB）；`>1` 时部署 N 个副本，前置 ALB，connect 隧道同样为 OHTTP 加密（`slb_type=alb` 与 `connect.encryption=ohttp` 固定不可换）。 |

### 步骤二：执行一键部署

在部署机上执行以下命令进入交互式部署：

```bash
curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/one-click/one-click/install-vllm-inference.sh | sh
```

脚本默认进入交互模式，会自动询问缺失的阿里云凭证、模型密钥文件、OSS 路径等必要参数。常用默认配置如下：

| 配置项               | 默认值                                                 | 说明                                          |
|----------------------|--------------------------------------------------------|-----------------------------------------------|
| Region               | `cn-beijing`                                           | 阿里云地域。                                  |
| Zone                 | `cn-beijing-l`                                         | 支持 gn8v-tee 实例规格的可用区。              |
| Instance Type        | `ecs.gn8v-tee.4xlarge`                                 | 默认 TDX 异构机密计算实例规格。               |
| System Disk          | `512G`                                                 | 镜像与运行态所需磁盘空间。                    |
| Replicas            | `1`                                                    | vLLM 副本数；单节点与多副本均使用 OHTTP 加密的 connect 隧道，`>1` 时额外前置 ALB。 |
| vLLM Version         | `0.19.1`                                               | vLLM 推理引擎版本。                           |
| vLLM Port            | `8080`                                                 | vLLM OpenAI 兼容 API 端口。                   |
| Served Model Name    | `Qwen3-32B`                                            | vLLM 对外暴露的模型名称。                     |
| Reference Value      | `rekor`                                                | 构建后将参考值上传 Rekor，部署时使用 Rekor 验证。 |
| Attestation Mode     | `challenge`                                            | 远程证明通过后才下发模型密钥。                |
| State Dir            | `$HOME/.confidential-agent`                            | 本地状态、密钥、Terraform 和构建产物目录。    |

非交互部署示例：

```bash
curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/one-click/one-click/install-vllm-inference.sh | sh -s -- \
  --non-interactive \
  --yes \
  --region cn-beijing \
  --zone-id cn-beijing-l \
  --instance-type ecs.gn8v-tee.4xlarge \
  --disk-gb 512 \
  --oss-bucket <bucket>:/<model-path> \
  --model-key-file <path-to-model-decryption-key>
```

多副本（N 个 vLLM 实例 + ALB + OHTTP 加密 connect 隧道）部署示例：

```bash
curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/one-click/one-click/install-vllm-inference.sh | sh -s -- \
  --non-interactive \
  --yes \
  --region cn-beijing \
  --zone-id cn-beijing-l \
  --instance-type ecs.gn8v-tee.4xlarge \
  --disk-gb 512 \
  --replicas 2 \
  --oss-bucket <bucket>:/<model-path> \
  --model-key-file <path-to-model-decryption-key>
```

`--replicas > 1` 时，spec 自动带上 `replicas: N`、`slb: {enabled: true, slb_type: alb}`、`connect: [{port: 8080, encryption: ohttp}]`；`confidential-agent connect` 通过 ALB 经 OHTTP 隧道访问推理服务，TDX 远程证明在隧道建立时完成。默认 `--replicas 1`（单节点）同样使用 OHTTP 加密 connect，但无 ALB，直连实例公网 IP。

### 步骤三：等待部署完成并确认

脚本会自动完成以下动作：

1. 安装部署机依赖。

2. 构建 `confidential-agent`、`confidential-agentd`、`cai-pep`，并将 `confidential-agent` 安装到 `/usr/local/bin`。

3. 构建 `confidential-agent-tools:latest` 镜像。

4. 构建 vLLM 推理可信镜像，启用 FDE（全盘加密）、dm-verity 和 UKI。

5. 将镜像参考值上传 Rekor 透明日志。

6. 创建阿里云云资源并启动 gn8v-tee TDX ECS 实例。

7. 远程证明通过后注入模型解密密钥（`model-decryption-key`）和运行时配置（`cai-model.env`）。

8. s3fs 挂载 OSS 模型密文，gocryptfs 在 TDX 内存内解密模型。

9. vLLM 读取解密后的模型启动推理服务。

10. 启动本地 TNG connect 隧道，执行 `/health` 可达性检查。

部署成功后，脚本会输出类似以下信息：

```plaintext
Confidential Agent one-click summary
  state_dir: /root/.confidential-agent
  work_dir:  /root/.confidential-agent/one-click
  spec:      /root/.confidential-agent/one-click/vllm-inference.yaml
  service:   vllm-inference
  region:    cn-beijing
  zone_id:   cn-beijing-l
  instance:  ecs.gn8v-tee.4xlarge
  cidrs:     203.0.113.10/32
  deployer:  203.0.113.10/32
  vllm:      curl http://127.0.0.1:8080/v1/models
  chat:      curl http://127.0.0.1:8080/v1/chat/completions \
                 -H 'Content-Type: application/json' \
                 -d '{"model":"Qwen3-32B","messages":[{"role":"user","content":"hi"}]}'
  connect:   running (pid 12345, log /root/.confidential-agent/one-click/connect.log)
```

**说明**

connect 隧道监听在部署机本地 `127.0.0.1:8080`。通过 `curl http://127.0.0.1:8080/v1/models` 可确认 vLLM 推理服务已就绪。

### 步骤四：访问推理服务

服务就绪后，通过 SSH 端口转发部署机的 connect 端口到本地，即可使用 OpenAI 兼容 API 发起推理请求。所有推理流量经 TNG OHTTP 通道加密进入 TDX 实例。

在个人电脑上执行以下命令建立 SSH 端口转发：

```bash
ssh -L 8080:127.0.0.1:8080 root@<DEPLOY_MACHINE_PUBLIC_IP>
```

发起非流式推理请求：

```bash
curl http://127.0.0.1:8080/v1/completions \
    -X POST \
    -H "Content-Type: application/json" \
    -d '{"model": "Qwen3-32B", "prompt": "Do you know the book Traction by Gino Wickman", "temperature": 0.0, "best_of": 1, "max_tokens": 132, "stream": false}'
```

发起流式推理请求：

```bash
curl http://127.0.0.1:8080/v1/completions \
    -X POST \
    -H "Content-Type: application/json" \
    -d '{"model": "Qwen3-32B", "prompt": "Do you know the book Traction by Gino Wickman", "temperature": 0.0, "best_of": 1, "max_tokens": 132, "stream": true}'
```

### 步骤五（可选）：释放资源

不再需要服务时，在部署机上执行以下命令释放云资源：

```bash
curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/one-click/one-click/install-vllm-inference.sh | sh -s -- cleanup \
  --state-dir "$HOME/.confidential-agent"
```

**警告**

销毁操作不可逆，会删除 ECS 实例、自定义镜像、OSS 对象、安全组、VPC、交换机等云资源。请确认不再需要实例中的数据后再执行。

安全验证
---------

### 验证供应链完整性

部署流程默认使用 Rekor 透明日志存储镜像参考值，远程证明过程中自动从 Rekor 获取日志条目进行验证。您可以通过以下方式对 Rekor 日志条目进行审计，验证供应链完整性。

构建完成后，可在状态目录下找到 Rekor metadata：

```bash
find "$HOME/.confidential-agent" -name "*.rekor-meta.json" -print
```

每个条目对应一份 SLSA provenance，包含 log index 和 entry URL。一键脚本输出的 Rekor 上传日志示例：

```plaintext
Created entry at index 1205944956, available at: https://rekor.sigstore.dev/api/v1/log/entries/<uuid>
```

验证包含性（inclusion proof），确认记录镜像参考值的条目确实存在于 Rekor 的默克尔树根中：

```bash
rekor-cli verify --log-index 1205944956 --rekor_server https://rekor.sigstore.dev
```

如果输出中包含两段相同的 hash 值，则说明验证成功：

```plaintext
Computed Root Hash: 1291abcee27148a4c00241ba8719f798ce060e8a5ccc8b18249017c25c6d0090
Expected Root Hash: 1291abcee27148a4c00241ba8719f798ce060e8a5ccc8b18249017c25c6d0090
```

验证一致性（consistency proof），确保 Rekor 上存储的参考值条目没有被删改：

```bash
rekor-cli loginfo --rekor_server https://rekor.sigstore.dev
```

输出中包含以下内容则说明验证成功：

```plaintext
Verification Successful!
```

### 验证运行环境可信

本方案采用 challenge 远程证明模式：部署时，实例必须先通过远程证明，验证运行环境（硬件 TDX、UKI 启动链完整性）符合预期参考值后，模型解密密钥才会被注入。未通过证明的实例无法获取模型密钥，因此无法解密模型。

部署完成后，可通过以下命令持续监控实例可信状态：

```bash
confidential-agent status --live
```

注意事项
---------

### 安全组规则

一键部署过程中将自动创建安全组规则。如需后续修改，请确保保留以下端口：

| 端口 | 协议 | 说明                                          | 建议                  |
|------|------|-----------------------------------------------|-----------------------|
| 22   | TCP  | SSH 远程管理（仅 Debug 镜像）                 | 限制源 IP 为管理网络。 |
| 8080 | TCP  | TNG 隧道端点（OHTTP），用于访问 vLLM 推理服务 | 限制源 IP 或仅通过 TNG 访问。 |

**重要**

默认 operator CIDR 为部署机公网出口 IP `/32`。如果手工指定 `0.0.0.0/0`，请同时降低暴露风险，并在生产环境改回具体公网出口或企业出口 CIDR。

### OSS 公共读密文安全模型

本方案中 OSS Bucket 存放的是 gocryptfs 加密后的模型密文，设置为公共读。密文在没有模型解密密钥的情况下无法解密，而模型解密密钥仅在远程证明通过后注入实例。因此，即使密文可被公开访问，也无法还原明文模型。

### 模型密钥轮换

如需轮换模型解密密钥，请使用新密钥重新加密模型并上传 OSS，然后使用新的 `--model-key-file` 重新运行一键部署脚本。

### 重启与密钥注入

challenge 模式下，模型解密密钥不跨 guest 重启持久化。实例重启后需重新执行 `confidential-agent inject` 完成密钥注入，方可恢复推理服务。

**生产场景如需 guest 重启自恢复与中心化密钥托管，可改用 `attestation.mode: trustee` 并自建长期运行的 Trustee 服务，参见 [docs/trustee.md](trustee.md)。**

常见问题
---------

### OSS 不可达或 s3fs 挂载失败

**原因** ：实例无法访问模型密文所在的 OSS，或 OSS Endpoint 配置不正确。

**解决** ：

* 确认 OSS Bucket 与实例在同一地域。

* 确认 `--oss-bucket` 参数格式为 `<bucket>:<path>`，路径指向 gocryptfs 密文根目录（包含 `gocryptfs.conf`）。

* 确认 `--oss-endpoint` 使用 internal 域名（默认 `https://oss-cn-beijing-internal.aliyuncs.com`）。

* 通过 debug SSH 登录 guest，查看 `journalctl -u cai-s3fs.service -n 200 --no-pager` 排查。

### gocryptfs 解密失败

**原因** ：模型解密密钥与加密时使用的密钥不匹配，或密文加密方式不是 gocryptfs。

**解决** ：

* 确认 `--model-key-file` 指向的密钥文件与加密模型时使用的 passphrase 完全一致。

* 确认 OSS 中的模型密文是通过 gocryptfs 加密生成（目录中应包含 `gocryptfs.conf`）。

* 通过 debug SSH 登录 guest，查看 `journalctl -u cai-gocryptfs.service -n 200 --no-pager` 排查。

### 远程证明未通过或密钥注入失败

**原因** ：实例运行环境与 Rekor 参考值不匹配，或证明服务不可达。

**解决** ：

* 确认部署时使用 `--reference-values rekor`（默认），且 Rekor 透明日志可访问。

* 查看 `connect.log` 与部署日志中远程证明相关输出。

* 通过 debug SSH 登录 guest，执行 `confidential-agent status --live` 查看证明状态。

### vLLM 未就绪

**原因** ：GPU 驱动未加载、模型未解密完成或 vLLM 服务启动失败。

**解决** ：

* 通过 debug SSH 登录 guest，确认 `/dev/nvidia0` 存在，GPU 驱动已加载。

* 确认模型已解密挂载：`ls /tmp/model/config.json`（默认挂载点）。

* 查看 `journalctl -u cai-vllm.service -n 200 --no-pager` 排查 vLLM 启动日志。

相关文档
---------

* [Trustee 远程证明服务部署](trustee.md)
* [系统架构](architecture.md)
* [设计规格](spec.md)
* 基于 g9i TDX 机密计算实例一键构建 OpenClaw 机密 AI Agent
* 基于 gn8v-tee 异构机密计算实例构建 OpenClaw 机密 AI Agent
