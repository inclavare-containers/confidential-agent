# Hermes Agent

This example builds a regular mkosi image for Hermes Agent on port `8642`.
During the mkosi post-install step, it clones the official Hermes Agent source
with GitHub fallback and runs the official installer with `--skip-browser`,
`--skip-setup`, and `--non-interactive`. At runtime,
`cai-hermes-agent.service` starts `/usr/local/bin/hermes gateway run` as the
`hermes` user. The AppSpec does not use containers.

The image includes the native `tdx-remote-attestation` skill under
`/opt/data/skills/tdx-remote-attestation` and the `cai-pep attest` helper.
There are no PEP execution patches or hooks in Hermes. Build-time download
adapters retain the existing domestic mirrors and hash verification. The service sets
`ATTESTATION_CHALLENGE_CLIENT_WORK_DIR=/opt/data/attestation` so its non-root
user can collect and verify evidence. Use that same environment variable when
running the helper manually as `hermes`.
For a source checkout, run `cargo build -p cai-pep` before building the example.
RPM-generated projects resolve the packaged helper automatically.

Before using it directly, create `secrets/hermes.env` and
`secrets/config.yaml` next to `hermes-agent.yaml`. They are injected as runtime
resources into `/opt/data`; they are not baked into the Shelter build config or
disk image.

The fixed Hermes source, verified toolchain, and dependency environment are
packaged under `/usr/local/lib/cai-hermes`. `/usr/local/lib/hermes-agent` links
to that source checkout. The package uses Hermes' supported payload manifest
so each service can keep its own `/opt/data` home without downloading another
runtime. The image disables lazy dependency installation and marks updates as
externally managed; update the image to change its fixed software version.

`secrets/hermes.env` should contain the API server and DashScope credentials:

```dotenv
API_SERVER_ENABLED=true
API_SERVER_HOST=0.0.0.0
API_SERVER_PORT=8642
API_SERVER_KEY=replace-with-openssl-rand-hex-32
DASHSCOPE_API_KEY=replace-with-dashscope-api-key
DASHSCOPE_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1
HERMES_HOME=/opt/data
HERMES_MODEL=qwen4-max
```

`secrets/config.yaml` should select the provider/model:

```yaml
model:
  provider: alibaba
  default: qwen4-max
  model: qwen4-max
```

`hermes_config` is marked `mutable: true` in the AppSpec because the Hermes
runtime may migrate and rewrite `config.yaml` on startup. The DashScope/API
secret file remains a normal enforced resource.

The root `install-hermes-agent.sh` script selects the Hermes source ref:

```bash
export HERMES_BRANCH='main'
export HERMES_COMMIT=''
```

Fresh installs fetch a small branch tip without checking out its files, then
extend that branch's history far enough to verify the pinned commit's ancestry.
Full history remains a fallback. The selected commit is checked out only after
that verification; a pin from another branch is rejected.

Set `CA_GITHUB_PROXY_URL` to override the GitHub fallback proxy. The default is
`https://gh-proxy.org/`. Set `HERMES_PYPI_INDEX_URL` or `UV_INDEX_URL` to
override the Python package index used by the official installer; the default is
`https://mirrors.aliyun.com/pypi/simple/`. Set `HERMES_NPM_REGISTRY_URL` or
`NPM_CONFIG_REGISTRY` to override the npm registry used by optional browser-tool
dependencies; the default is `https://registry.npmmirror.com/`.
`PLAYWRIGHT_DOWNLOAD_HOST` defaults to `https://npmmirror.com/mirrors/playwright`.
Installer curl downloads have a 20-second connection timeout and a 180-second
total timeout, allowing the upstream hash-verified artifact mirror to handle
unavailable GitHub downloads. Override these with
`HERMES_DOWNLOAD_CONNECT_TIMEOUT_SEC` and `HERMES_DOWNLOAD_TIMEOUT_SEC`.

The build uses every stage declared by the official installer. After repository
checkout, it maps frozen PyPI artifact URLs to
`https://mirrors.aliyun.com/pypi/packages/`; package versions, SHA-256 hashes,
registry identity and dependency markers stay unchanged. Set
`HERMES_PYPI_ARTIFACT_MIRROR` to another HTTPS artifact prefix, or to an empty
value to retain the original `files.pythonhosted.org` URLs. Lock digests before
and after this transport change are recorded in
`/usr/local/lib/cai-hermes/locked-downloads.json`.

Hermes PM downloads pinned tools such as Python, FFmpeg and ripgrep with its own
Python downloader, independently of curl/git and the PyPI index. During build,
the installer adapts its GitHub binary transport to try `CA_GITHUB_PROXY_URL`
first, then the original URL and the official content-addressed mirror. The
default proxy is `https://gh-proxy.org/`. Versions and lockfile SHA-256 hashes
remain unchanged; PM still verifies the downloaded bytes before publishing them.
The adapter change is recorded in `/usr/local/lib/cai-hermes/binary-downloads.json`.
Non-GitHub binary URLs keep their original download behavior. This applies to
both the standalone Hermes image and all-in-one builds.
