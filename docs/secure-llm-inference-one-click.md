基于异构机密计算实例构建安全大语言模型推理环境
================================================

随着大语言模型技术的普及，企业对推理过程中的数据安全问题日益关注。模型权重是核心资产，推理 Prompt 与回复可能包含敏感业务数据，模型解密密钥一旦泄露将导致模型保护失效。本文介绍如何在阿里云 gn8v-tee TDX 机密计算实例上，使用一键部署脚本快速构建基于 vLLM 的安全大语言模型推理环境，通过Intel TDX 硬件级内存加密和隔离、模型密钥分发、远程证明和端到端加密，保护模型权重、推理数据和模型密钥的安全。

方案概述
---------

本方案将 vLLM 推理服务封装在 Intel TDX（Trust Domain Extensions）可信执行环境中。模型以加密密文形式存储在 OSS Bucket 中，仅在远程证明通过后将模型解密密钥注入实例，由 gocryptfs 在 TDX 内存边界内完成解密挂载，vLLM 直接读取明文模型启动推理。用户推理请求经可信网关（TNG）提供的加密通道进入 TDX 实例，CPU 侧的模型解密和请求处理位于 TDX 来宾内；GPU 侧计算使用启用 NVIDIA Confidential Compute 的 GPU 环境。

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
| 运行时    | vLLM 的 CPU 侧逻辑运行在 TDX 信任域内，GPU 侧使用 NVIDIA Confidential Compute。                    |
| 密钥管理   | 模型解密密钥与运行时配置仅在远程证明通过后注入，challenge 模式确保密钥不下发到未证明实例。     |
| 通信链路   | 采用基于HPKE的端到端信封加密协议，远程证明验证实例身份后才建立通道，推理流量全程加密传输。                |

当前 challenge/Trustee 证明链校验 TDX 来宾和 UKI 度量。GPU CC 由启动脚本配置，debug 验收检查其运行状态；当前取密钥策略未绑定独立的 GPU 远程证明结果。

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

  推理镜像显式配置 EPEL 8 仓库以安装 `openssl3`，并保留 RPM GPG 校验；构建环境需要能够访问该仓库及其签名公钥。

* （可选）如需使用自有模型，请在步骤一中完成 gocryptfs 加密与 OSS 上传；也可直接使用步骤一提供的预加密 Qwen3.8-27B 模型跳过该操作。

操作步骤
---------

**说明**

本文以 `ecs.gn8v-tee.4xlarge` 实例规格、地域 `cn-beijing`、可用区 `cn-beijing-l` 为默认配置。

### 步骤一：准备加密模型

本方案的模型以密文形式存储在 OSS 中。请在部署前完成模型加密与上传。该步骤包括下载模型、加密模型和上传模型三个环节，由于模型文件较大，整个过程耗时较长，取决于您的部署机网络配置。

**说明**

如果您希望快速体验此方案，可以使用我们为您准备的加密模型文件进行试用。选择下表中的预加密模型后，您将能够跳过该步骤，直接进入[步骤二：准备凭证](#步骤二准备凭证)。

使用演示模型时，仍需先创建部署参数引用的演示密钥文件：

```bash
umask 077
printf '%s\n' '0Bn4Q1wwY9fN3P' > "$HOME/cachefs-password"
```

此密码公开且仅用于演示，不提供自有模型的保密性；自有模型应使用独立生成的密钥。

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
      cp -r "${PLAINTEXT_MODEL_PATH}/." ~/mount/plain

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

### 步骤三：安装与部署

安装脚本只安装 RPM，部署参数、凭证、等待和连接统一由 CLI 处理：

```bash
curl -fSL https://raw.githubusercontent.com/inclavare-containers/confidential-agent/master/one-click/install.sh -o /tmp/install-cagent.sh
sh /tmp/install-cagent.sh
cagent up vllm-inference --oss-bucket YOUR_BUCKET \
  --model-key-file /private/model-decryption-key --output-dir ./projects
```

CLI 读取 `~/.aliyun/config.json` 或云凭证环境变量。未配置时，交互模式隐藏输入并保存凭证。`--oss-endpoint` 控制模型下载端点；镜像发布上传使用独立的 `CA_IMAGE_OSS_ENDPOINT`。

已有签名镜像时跳过构建：

```bash
cagent up vllm-inference --stage deploy --service-id inference \
  --image-release ./image-release.json --trusted-key ./publisher.pem \
  --oss-bucket YOUR_BUCKET --model-key-file /private/model-decryption-key \
  --replicas 3 --output-dir ./projects
```

分阶段多副本模板使用 ALB + OHTTP。默认放行部署机出口 `/32`；可用 `--deployer-cidr` 指定部署机网段，并用 `--allowed-cidr` 添加客户端网段。重用项目执行 `cagent up --spec ./projects/inference/instance.yaml`，不重新生成密钥和配置。

### 步骤四：状态与访问

`up` 等待部署激活，再调用 `connect start` 创建受保护连接；失败返回错误并保留实际状态。单独检查：

```bash
cagent status --service inference --live --json
cagent tui --service inference
```

连接输出中的 `client_endpoints` 给出实际本地地址，不假设端口一定是 8080。连接记录和日志位于 `<state-dir>/services/inference/connect-ready.json` 及相邻 `.log` 文件。通过该地址访问 `/v1/models` 和 `/v1/chat/completions`。没有 `--service-id` 的独立模板默认服务名为 `vllm-inference`。

### 步骤五：释放资源

```bash
cagent cleanup inference
# 查看当前状态目录中的所有实例：
cagent cleanup --all --dry-run
# 销毁上述所有实例：
cagent cleanup --all
```

销毁保留发布镜像与本地状态。镜像发布者使用 `image unpublish` 管理共享镜像生命周期。

### 证明与供应链

`sample` 使用构建或签名发布元数据中的实测参考值，不表示跳过远程证明。`rekor` 使用公开的参考值元数据；当前 challenge 流程注册 Rekor 参考值失败时会尝试本地实测值。`rekor.required` 约束构建发布，不能解读为所有后续连接都必须在线访问 Rekor。字段与恢复语义见 [AppSpec 文档](spec.md)。
