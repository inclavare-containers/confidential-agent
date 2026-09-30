基于异构机密计算实例构建安全大语言模型推理环境
================================================

随着大语言模型技术的普及，企业对推理过程中的数据安全问题日益关注。模型权重是核心资产，推理 Prompt 与回复可能包含敏感业务数据，模型解密密钥一旦泄露将导致模型保护失效。本文介绍如何在阿里云 gn8v-tee TDX 机密计算实例上，使用一键部署脚本快速构建基于 vLLM 的安全大语言模型推理环境，通过Intel TDX 硬件级内存加密和隔离、模型密钥分发、远程证明和端到端加密，保护模型权重、推理数据和模型密钥的安全。

方案概述
---------

本方案将 vLLM 推理服务封装在 Intel TDX（Trust Domain Extensions）可信执行环境中。模型以加密密文形式存储在 OSS Bucket 中，仅在远程证明通过后将模型解密密钥注入实例，由 gocryptfs 在 TDX 内存边界内完成解密挂载，vLLM 直接读取明文模型启动推理。用户推理请求经可信网关（TNG）提供的加密通道进入 TDX 实例，推理 Prompt 与回复全程在 TDX 内存加密边界内流转。

本方案通过定制化ECS实例系统镜像的方式部署，其中系统镜像为不可变（immutable）镜像，仅包含推理业务所需的最简依赖且禁用外部命令执行接口，降低攻击面，同时提供镜像安全加固和对业务代码的完整度量能力。部署时，先通过远程证明验证实例可信，验证通过后才下发模型解密密钥与运行时配置，确保模型密钥不会注入到未经证明的实例中。

### 安全架构

部署中的核心数据均在 TEE（Trusted Execution Environment，可信执行环境）边界内闭环流转，以下资产受到保护：

| 保护对象   | 说明                                                                                       |
|------------|--------------------------------------------------------------------------------------------|
| 推理数据   | 用户推理 Prompt 与模型回复，可能包含个人身份、医疗、金融等敏感信息。                       |
| 模型权重   | 以密文存储于 OSS，仅在 TDX 内存内解密，云平台和宿主机无法读取明文。             |
| 模型密钥   | 模型解密密钥，仅在远程证明通过后注入实例，未证明环境无法获取。                       |

安全架构自底向上覆盖硬件、启动链、运行时、密钥和通信五个层级：

| 保护层级 | 机制                                                                                          |
|----------|-----------------------------------------------------------------------------------------------|
| 硬件层    | Intel TDX 对 Guest OS 全部内存透明加密，云平台和宿主机无法读取明文。                          |
| 启动链    | UKI（Unified Kernel Image，统一内核镜像）和 dm-verity rootfs 防篡改；镜像度量值上传 Rekor 透明日志。 |
| 运行时    | vLLM 推理服务运行在 TDX 信任域内，模型解密与推理全程在内存加密边界内完成。                    |
| 密钥管理   | 模型解密密钥与运行时配置仅在远程证明通过后注入，challenge 模式确保密钥不下发到未证明实例。     |
| 通信链路   | 采用基于HPKE的端到端信封加密协议，远程证明验证实例身份后才建立通道，推理流量全程加密传输。                |

### 部署架构

部署流程如下：

1. **准备加密模型（可选）** ：下载模型并使用 gocryptfs 加密，将密文上传到公共读 OSS Bucket；也可使用提供的预加密 Qwen3.8-27B 模型跳过该步。

2. **凭证准备** ：在部署机上导出阿里云 AccessKey，准备模型解密密钥文件和 OSS 模型密文路径。

3. **一键部署** ：执行一键部署脚本，自动完成依赖安装、可信镜像构建、参考值上链、云资源创建、远程证明和机密资源注入。

4. **建立加密通道** ：脚本自动启动 TNG 加密隧道，建立部署机与 TDX 实例之间的加密通道。

5. **访问推理服务** ：在部署机上使用 OpenAI 兼容 API 发起推理请求，所有流量经加密通道进入 TDX 实例。目标实例禁用 SSH/远程登录，无需也无法登录实例。

前提条件
---------

* 已开通阿里云 ECS 服务，账号具备创建 TDX 机密计算实例、VPC、交换机、安全组、OSS Bucket、自定义镜像等资源的权限。

* 已准备一台 Alibaba Cloud Linux 3 ECS 实例作为部署机，可用磁盘空间不低于 200 GB（如在该部署机上执行步骤一的模型加密，需额外容纳明文与密文两份模型拷贝）。

  **重要**

  部署机仅用于执行构建和部署操作，后续一键脚本会自动创建新的 TDX 机密实例承载 vLLM 推理服务。部署机本身无需是 TDX 实例。

* 已获取阿里云访问凭证。建议使用 RAM 用户最小权限、RAM 角色或 STS 临时凭证，避免长期存储主账号 AccessKey。

* 部署机可以访问公网，用于拉取源码、系统包、Rekor 透明日志和阿里云 OpenAPI。

* （可选）如需使用自有模型，请在步骤一中完成 gocryptfs 加密与 OSS 上传；也可直接使用步骤一提供的预加密 Qwen3.8-27B 模型跳过该操作。

操作步骤
---------

**说明**

本文以 `ecs.gn8v-tee.4xlarge` 实例规格、地域 `cn-beijing`、可用区 `cn-beijing-l` 为默认配置。

### 步骤一：准备加密模型

本方案的模型以密文形式存储在 OSS 中。请在部署前完成模型加密与上传。该步骤包括下载模型、加密模型和上传模型三个环节，由于模型文件较大，整个过程耗时较长，取决于您的部署机网络配置。

**说明**

如果您希望快速体验此方案，可以使用我们为您准备的加密模型文件进行试用。选择下表中的预加密模型后，您将能够跳过该步骤，直接进入[步骤二：准备凭证](#步骤二准备凭证)。

| 模型名称 | 加密方式 | 加密密码 | OSS地域 | 模型存储位置 |
|----------|----------|----------|---------|--------------|
| Qwen3.8-27B | Gocryptfs | 0Bn4Q1wwY9fN3P | cn-beijing | conf-ai:/qwen3.8-27b-gocryptfs/ |

#### 1.下载模型

部署前，需先加密模型并上传到云存储。请在本地或可信环境中完成模型加密。以部署 Qwen3.8-27B 模型为例，指导如下。

**说明**

如果您已经拥有自己的模型，请跳过本章节，直接进入 2.加密模型 章节，并将后续示例中的模型路径替换为目标模型的实际路径。

以使用 modelscope 工具下载 Qwen3.8-27B 模型为例（需要 Python 3.9 或更高版本），在终端中执行如下命令下载模型。

```bash
pip3 install modelscope importlib-metadata
modelscope download --model Qwen/Qwen3.8-27B
```

成功执行命令后，模型将被下载到 `~/.cache/modelscope/hub/models/Qwen/Qwen3.8-27B/` 目录下。

#### 2.加密模型

本方案采用 Gocryptfs 加密方式：基于 AES256-GCM，符合开源 Gocryptfs 标准的加密模式。

1. 安装 Gocryptfs 加密工具（仅支持默认参数的 v2.4.0 版本），选择以下任意一种方式安装：

   **方式一：（推荐）从 yum 源安装**

   如果您使用 Alinux 3 或者 AnolisOS 23 操作系统，可以直接利用 yum 源安装 gocryptfs。

   **Alinux 3**

   ```bash
   sudo yum install gocryptfs -y
   ```

   **AnolisOS 23**

   ```bash
   sudo yum install anolis-epao-release -y
   sudo yum install gocryptfs -y
   ```

   **方式二：直接下载预编译 binary**

   ```bash
   # 下载预编译 Gocryptfs 压缩包
   wget https://github.jobcher.com/gh/https://github.com/rfjakob/gocryptfs/releases/download/v2.4.0/gocryptfs_v2.4.0_linux-static_amd64.tar.gz

   # 解压并安装
   tar xf gocryptfs_v2.4.0_linux-static_amd64.tar.gz
   sudo install -m 0755 ./gocryptfs /usr/local/bin
   ```

2. 创建 Gocryptfs 密钥文件作为模型加密密钥。后续部署时需通过 `--model-key-file` 将该密钥传入实例。

   本方案使用 `0Bn4Q1wwY9fN3P` 作为加密模型使用的密钥，密钥内容存储在 `cachefs-password` 文件中。在实际生产中，建议使用随机生成的强密钥。

   ```bash
   cat << EOF > ~/cachefs-password
   0Bn4Q1wwY9fN3P
   EOF
   ```

3. 使用已创建的密钥加密模型。

   1. 配置明文模型的路径。

      ```bash
      PLAINTEXT_MODEL_PATH=~/.cache/modelscope/hub/models/Qwen/Qwen3.8-27B/
      ```

   2. 使用 Gocryptfs 对模型目录树进行加密。加密完成后，模型将以密文形式存储在 `~/mount/cipher` 目录中。

      ```bash
      mkdir -p ~/mount
      cd ~/mount
      mkdir -p cipher plain

      # 安装 Gocryptfs 运行时依赖
      sudo yum install -y fuse

      # initialize gocryptfs
      cat ~/cachefs-password | gocryptfs -init cipher

      # mount to plain
      cat ~/cachefs-password | gocryptfs cipher plain

      # move AI model to ~/mount/plain
      cp -r ${PLAINTEXT_MODEL_PATH}/. ~/mount/plain

      # unmount after encryption is complete
      fusermount -u ~/mount/plain
      ```

#### 3.上传模型

您需要准备一个与即将部署的实例相同地域的 OSS Bucket，并将加密模型上传到阿里云 OSS 对象存储，以便后续从实例拉取并部署。因模型文件较大，推荐使用 ossbrowser 将加密模型上传到该目录。Bucket 需设置为公共读，以便实例内 s3fs 以匿名方式挂载密文。

### 步骤二：准备凭证

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
| `--model-key-file`  | 部署机上的模型解密密钥文件路径（必须存在，部署时注入）。 |
| `--oss-bucket`      | OSS Bucket 中模型密文路径，格式为 `<bucket>:<path>`。       |
| `--oss-endpoint`    | OSS Endpoint，默认 `https://oss-cn-beijing-internal.aliyuncs.com`。 |
| `--replicas`        | vLLM 副本数，默认 `1`（只创建单节点实例，不创建ALB网关）；`>1` 时部署 N 个副本，前置提供ALB网关将推理请求负载均衡到N个节点）。 |

### 步骤三：执行一键部署

在部署机上执行以下命令进入交互式部署：

```bash
curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/secure-llm-inference/one-click/install-vllm-inference.sh | sh
```

脚本默认进入交互模式，会自动询问缺失的阿里云凭证、模型密钥文件、OSS 路径等必要参数。常用默认配置如下：

| 配置项               | 默认值                                                 | 说明                                          |
|----------------------|--------------------------------------------------------|-----------------------------------------------|
| Region               | `cn-beijing`                                           | 阿里云地域。                                  |
| Zone                 | `cn-beijing-l`                                         | 支持 gn8v-tee 实例规格的可用区。              |
| Instance Type        | `ecs.gn8v-tee.4xlarge`                                 | 默认 TDX 异构机密计算实例规格。               |
| System Disk          | `512G`                                                 | 镜像与运行态所需磁盘空间。                    |
| Replicas            | `1`                                                    | 运行vLLM的gn8v-tee实例副本数。                                |
| vLLM Version         | `0.19.1`                                               | vLLM 推理引擎版本。                           |
| vLLM Port            | `8080`                                                 | vLLM OpenAI 兼容 API 端口。                   |
| Served Model Name    | `Qwen3.8-27B`                                            | vLLM 对外暴露的模型名称。                     |
| Reference Value      | `rekor`                                                | 构建后将参考值上传 Rekor，部署时使用 Rekor 验证。 |
| Attestation Mode     | `challenge`                                            | 远程证明通过后才下发模型密钥。                |
| State Dir            | `$HOME/.confidential-agent`                            | 本地状态、密钥、Terraform 和构建产物目录。    |

以下示例使用步骤一提供的预加密 Qwen3.8-27B 模型（`conf-ai:/qwen3.8-27b-gocryptfs/`）与演示密钥文件 `~/cachefs-password`；如使用自有模型，请将 `--oss-bucket`、`--model-key-file` 与 `--vllm-served-model-name` 替换为实际值。

非交互部署示例：

```bash
curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/secure-llm-inference/one-click/install-vllm-inference.sh | sh -s -- \
  --non-interactive \
  --yes \
  --region cn-beijing \
  --zone-id cn-beijing-l \
  --instance-type ecs.gn8v-tee.4xlarge \
  --disk-gb 512 \
  --oss-bucket conf-ai:/qwen3.8-27b-gocryptfs/ \
  --vllm-served-model-name Qwen3.8-27B \
  --model-key-file ~/cachefs-password
```

多副本（N 个 运行vLLM的gn8v-tee实例 + 1个ALB）部署示例：

```bash
curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/secure-llm-inference/one-click/install-vllm-inference.sh | sh -s -- \
  --non-interactive \
  --yes \
  --region cn-beijing \
  --zone-id cn-beijing-l \
  --instance-type ecs.gn8v-tee.4xlarge \
  --disk-gb 512 \
  --replicas 2 \
  --oss-bucket conf-ai:/qwen3.8-27b-gocryptfs/ \
  --vllm-served-model-name Qwen3.8-27B \
  --model-key-file ~/cachefs-password
```

### 步骤四：等待部署完成并确认

脚本会自动完成以下动作：

1. 构建和安装部署机依赖。

2. 构建gn8v-tee实例使用的系统镜像，内置vLLM推理服务，启用全盘加密和度量能力。

3. 在本地对镜像进行度量，并将镜像参考值上传Rekor的公共透明日志服务。

4. 创建阿里云云资源并启动 gn8v-tee TDX ECS 实例（如果设置`replicas>1`则将创建多个副本，并创建一个额外的ALB负载均网关实例）。

5. 远程证明验证实例通过后注入模型解密密钥和运行时配置。

6. gn8v-tee实例从阿里云OSS挂载模型密文数据，并在实例内按需解密模型密文。

7. gn8v-tee实例启动vLLM进程，并加载解密后的模型，启动推理服务。

8. 部署机启动本地 TNG 隧道。

部署成功后，脚本会输出类似以下信息：

```plaintext
Deployment summary
  state_dir: /root/.confidential-agent
  work_dir:  /root/.confidential-agent/one-click
  spec:      /root/.confidential-agent/one-click/vllm-inference/vllm-inference.yaml
  service:   vllm-inference
  region:    cn-beijing
  zone_id:   cn-beijing-l
  instance:  ecs.gn8v-tee.4xlarge
  cidrs:     203.0.113.10/32
  deployer:  203.0.113.10/32
  vllm:      curl http://127.0.0.1:8080/v1/models
  chat:      curl http://127.0.0.1:8080/v1/chat/completions \
                 -H 'Content-Type: application/json' \
                 -d '{"model":"Qwen3.8-27B","messages":[{"role":"user","content":"hi"}]}'
  connect:   running (pid 12345, log /root/.confidential-agent/one-click/connect-vllm-inference.log)
```

**说明**

TNG隧道监听在部署机本地 `127.0.0.1:8080`。通过 `curl http://127.0.0.1:8080/v1/models` 可确认 vLLM 推理服务已就绪。

### 步骤五：访问推理服务

* **在部署机上访问**

  部署脚本已在部署机上自动启动TNG隧道，监听本地 `127.0.0.1:8080`。可直接使用下方 `curl` 示例发起请求，所有推理流量均通过TNG隧道进入 TDX 实例。

  查看模型列表：

  ```bash
  curl http://127.0.0.1:8080/v1/models
  ```

  发起非流式对话推理请求：

  ```bash
  curl http://127.0.0.1:8080/v1/chat/completions \
      -X POST \
      -H "Content-Type: application/json" \
      -d '{"model": "Qwen3.8-27B", "messages": [{"role": "user", "content": "你好"}], "temperature": 0.0, "max_tokens": 132, "stream": false}'
  ```

  发起流式推理请求：

  ```bash
  curl http://127.0.0.1:8080/v1/chat/completions \
      -X POST \
      -H "Content-Type: application/json" \
      -d '{"model": "Qwen3.8-27B", "messages": [{"role": "user", "content": "你好"}], "temperature": 0.0, "max_tokens": 132, "stream": true}'
  ```

  隧道在建立时完成TDX远程证明，确认对端为可信的 TDX 实例后，推理 Prompt 与回复经端到端的HPKE信封加密算法传输。


* **从其他客户端（如个人电脑/手机端）访问**

为方便演示，我们对可信网关TNG隧道的实现做了封装，需先将部署机的服务状态目录（默认 `$HOME/.confidential-agent`）拷贝到该机器，再在该机器上运行以下命令启动隧道监听（默认 `127.0.0.1:8080`）：

  ```bash
  confidential-agent connect start --service vllm-inference
  ```

随后便可和部署机一样的方式访问推理服务。

如需集成可信网关客户端到您的业务中来发起推理请求，我们支持如下几种访问方式，您可以结合您的业务场景进行选择不同的集成方式

| 访问方式 | 获取方式 |
| --- | --- |
| 代理网关（HTTP代理/socks5代理/透明代理）| 可从[GitHub仓库](https://github.com/inclavare-containers/TNG/releases/tag/v2.9.2)获取二进制/容器，[文档及示例](https://github.com/inclavare-containers/TNG/blob/v2.9.2/docs/configuration_zh.md) |
| Python SDK | `pip install tng-sdk==2.9.2`，[文档及示例](https://github.com/inclavare-containers/TNG/tree/v2.9.2/tng-python) |
| Go SDK | `go get github.com/inclavare-containers/tng/tng-go`，[文档及示例](https://github.com/inclavare-containers/TNG/tree/v2.9.2/tng-go) |
| JS SDK | `npm i @inclavare-containers/tng@2.9.2`，[文档及示例](https://github.com/inclavare-containers/TNG/tree/v2.9.2/tng-wasm) |

### 步骤六（可选）：释放资源

不再需要服务时，在部署机上执行以下命令释放云资源：

```bash
curl -fsSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/secure-llm-inference/one-click/install-vllm-inference.sh | sh -s -- cleanup \
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

注意事项与常见问题
---------

- **排查问题不用登录实例**：目标实例不允许 SSH 登录。出了问题在部署机上运行 `confidential-agent status --live` 查看实例内部状态，或查看 `$HOME/.confidential-agent/one-click/` 目录下的部署日志（特别是 `connect-vllm-inference.log`）。

- **安全组规则**：部署脚本会自动配置安全组，开放 8080 端口用于加密推理访问。实例不开放 SSH 端口。默认只允许部署机的公网 IP 访问实例；生产环境请保持这一限制，不要改成允许所有 IP。

- **模型密文可以公开访问**：OSS 里存的是加密后的模型，即使 Bucket 设为公共读，没有解密密钥也无法还原。解密密钥只在实例通过远程证明后才会注入。

- **实例拉不到模型**：确认 OSS Bucket 和实例在同一个地域；`--oss-bucket` 参数格式为 `<bucket>:<path>`，指向加密模型的根目录；`--oss-endpoint` 使用内网域名（默认值即可）。

- **模型解密失败**：`--model-key-file` 指定的密钥文件必须和加密模型时用的密码完全一致；确认 OSS 里的模型是用 gocryptfs 加密的（目录下应有 `gocryptfs.conf` 文件）。

- **远程证明没通过**：确认使用了 `--reference-values rekor`（默认），并且 Rekor 服务可以访问；查看部署日志中验证相关的输出。

- **vLLM 没启动好**：用 `confidential-agent status --live` 查看实例内服务状态；部署脚本在加密通道建立后会做一次健康检查，结果在日志里。实例没有 SSH，看不了系统日志，vLLM 的启动细节请查看部署日志。

- **实例重启后推理不可用**：默认模式下，模型解密密钥不会在重启后保留，需要重新运行 `confidential-agent inject` 注入。如果需要重启自动恢复，可以改用 Trustee 模式（参见 [docs/trustee.md](trustee.md)）。

- **更换模型密钥**：用新密码重新加密模型、上传到 OSS，然后用新的 `--model-key-file` 重新部署。


相关文档
---------

* [基于 gn8v-tee 异构机密计算实例一键构建 OpenClaw 机密 AI Agent](https://help.aliyun.com/zh/egs/use-cases/build-openclaw-confidential-ai-agent-on-heterogeneous-confidential-computing-instances)
* [基于 gn8v-tee 关联实例构建安全大语言模型推理环境](https://help.aliyun.com/zh/egs/use-cases/build-a-secure-llm-inference-environment-on-gn8v-tee-related-instances)
