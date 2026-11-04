# Shared allinone image

Build one release image containing OpenClaw, Hermes, QoderCLI, OpenCode, Codex,
and Claude Code. Each deployed VM selects one harness using the image's
`runtime.json`. CA accepts ordinary AppSpecs and resources for both this image
and custom images; it has no allinone harness enum or model flags.

The existing CA FDE policy protects the root filesystem's integrity and encrypts
the writable delta. CA continues to verify TDX evidence and release keys and
resources through challenge or Trustee. This example uses challenge/sample;
Rekor and Trustee require their usual operator configuration.

## Initialize from the installed RPM

Run `confidential-agent init` for the interactive build/deploy wizard, or:

```bash
confidential-agent init allinone --stage build --non-interactive
cd confidential-agent-init/allinone-builder
```

The generated directory is self-contained and includes `NEXT_STEPS.md` for
building, publishing and exporting a signed release. No model credentials are
needed for image initialization. Software versions stay in runtime-contract.json.

For each consumer, run `confidential-agent init allinone --stage deploy`: choose a unique
service ID, the signed release and trusted publisher key, then a harness and mode. The manifest digest is read automatically; `--manifest` is an optional version pin. Use the same release for multiple independent
services, including different harnesses. Initialization does not create cloud
resources. Generated consumer projects have no build inputs.
See [RPM walkthrough](../../docs/rpm.md) for the non-interactive commands.

Mode choices depend on the selected harness:

| Harness | Mode | How to use it |
| --- | --- | --- |
| Codex | `cli` (default) | SSH into the instance and run `cai-agent` in a terminal |
| Codex | `app-server` | Connect a remote Codex client through the CA tunnel to the guest Codex server |
| OpenClaw / Hermes | `gateway` (default) | Run a persistent network service for clients |
| OpenClaw / Hermes | `cli` | Run the harness through SSH |
| Claude Code / QoderCLI / OpenCode | `cli` | Run the harness through SSH; init selects this automatically |

Codex has no `gateway` mode in this project. The separately installed
`cai-gateway` handles CA mesh traffic and is not a Codex mode.
For ordinary terminal use of Codex, accept `cli`.

Every harness includes the `tdx-remote-attestation` skill and uses
`/usr/local/bin/cai-pep attest collect-and-verify` to verify TDX evidence.
The native skill directories are:

| Harness | Skill directory |
| --- | --- |
| Codex | `/opt/data/allinone/home/.agents/skills/tdx-remote-attestation` |
| Claude Code | `/opt/data/allinone/home/.claude/skills/tdx-remote-attestation` |
| QoderCLI | `/opt/data/allinone/home/.qoder/skills/tdx-remote-attestation` |
| OpenCode | `/opt/data/allinone/home/.config/opencode/skills/tdx-remote-attestation` |
| Hermes | `/opt/data/allinone/hermes/skills/tdx-remote-attestation` |
| OpenClaw | `/opt/data/allinone/home/.openclaw/skills/tdx-remote-attestation` |

OpenClaw includes the executor patch, PEP plugin, policy service and Podman
sandbox, as do the standalone OpenClaw and OpenClaw-vLLM examples. Selecting
OpenClaw starts its PEP service; startup fails if that service cannot start.
The other harnesses use only the attestation helper and native skill loading;
they have no PEP execution hooks or patches. The existing build-time download
adapters for domestic mirrors are retained.

The image also installs `procps-ng`: interactive Codex uses `ps`
to manage its background app-server, even when the selected harness mode is `cli`.
These files are image build inputs. Updating the controller RPM does not add
them to an existing image; rebuild, publish and export a new signed release.

## Use a prebuilt image

Obtain the signed release and an independently trusted publisher public key.
Copy `instance.yaml` and configure its release/key paths and region. Use a
different `service.id` for every VM. Configure operator peering with SSH scope
before deploying. `service.ssh: true` creates unique client and host keys;
CA delivers them as attested resources and pins the host key for `confidential-agent ssh`.
The image starts SSH only after those resources arrive. No SSH key is baked
into the shared image.
Keep `service.ssh: true` in the build specification too: it selects Cryptpilot's
SSH-preserving hardening mode while retaining disk encryption and integrity.
The default SSH service and socket stay disabled.

```bash
confidential-agent spec validate --spec instance.yaml
confidential-agent deploy --spec instance.yaml
confidential-agent ssh allinone-codex -- cai-agent --version
confidential-agent ssh allinone-codex --tty -- cai-agent
confidential-agent destroy allinone-codex
```

Consumer destruction preserves the shared cloud image. `image list` shows builders
and their publications; `status` shows instances. `image rm BUILDER` removes local
image payloads while retaining publication records. Use `forget INSTANCE` only
after destroying the instance to remove its local management records. The publisher owns
`image unpublish` and its import snapshots. Each consumer owns its Terraform
state, gateway identity, SSH keys, disk passphrase, resources and writable data.
The example deploys a 120 GB disk for the 80 GB base image: Cryptpilot allocates
the remaining volume-group space to the encrypted writable delta. Keep this
headroom when choosing the deployment disk size.

Cleanup checks all consumers and imported releases visible in the publisher's
workspace. Exporting a signed release records retention: even `image prune --all`
keeps that cloud image. Before retiring a release, stop distributing it, allow
your advertised retention period to expire, and confirm that local and remote
consumers no longer need it for creation or recovery. Then explicitly run
`image unpublish SERVICE --image-id IMAGE_ID --force`. The publisher cannot
discover consumers in another workspace or on another host. Cleanup failures
return a nonzero exit status and retain failed entries for retry.

## Image configuration

`runtime.json` accepts `harness` (`openclaw`, `hermes`, `qodercli`, `opencode`,
`codex`, `claude-code`), an optional `mode`, an optional listening `port`,
and a map of string environment variables. Existing configurations retain their
default mode: gateway for OpenClaw/Hermes and CLI for the other harnesses.
Deliver it to `/opt/data/allinone/runtime.json`. API credentials may be included
in this private resource or in the harness's native configuration. Models,
provider URLs and authentication use the harness's official configuration:

| Harness | Native configuration under the encrypted instance data |
|---|---|
| OpenClaw | `/opt/data/allinone/home/.openclaw/openclaw.json`; gateway defaults to 18789; `port` overrides it |
| Hermes | `/opt/data/allinone/hermes/config.yaml` and `.env`; service runs `hermes gateway run` |
| QoderCLI | `/opt/data/allinone/home/.qoder`; optional `QODER_PERSONAL_ACCESS_TOKEN` in environment |
| OpenCode | `/opt/data/allinone/home/.config/opencode/opencode.json` |
| Codex | `/opt/data/allinone/home/.codex/config.toml`; `OPENAI_API_KEY` in environment |
| Claude Code | `/opt/data/allinone/home/.claude/settings.json` and its supported provider environment |

Change the resource entries and application ports in `instance.yaml` to match
the selected harness. CLI harnesses use SSH and `service.ports: []`; their
readiness unit stays active while interactive commands run through `cai-agent`.
`app_ready` reports the unit and declared listening ports; it does not certify
model credentials or a successful model request. Run the image-local
`cai-agent --check` for configuration/port checks and an explicit model smoke
test for model connectivity. Application logs stay in the guest volatile journal
and are lost on reboot. They are not forwarded to the cloud console.
The daemon supplies the application runtime context after verifying and applying
the selected provider's resources. The launcher does not read challenge or
Trustee caches. Management SSH starts independently when its own attested
credentials are ready, so application configuration errors can be diagnosed
with `confidential-agent ssh SERVICE -- journalctl -u cai-allinone.service --no-pager`.
OpenClaw and Hermes run only the selected gateway. All per-harness units from
individual installers remain disabled.

Resource `mutable` selects content ownership. The default `false` is
operator-managed: the daemon restores guest edits to the delivered content.
`true` is guest-managed after initialization: re-injecting an updated source
does not replace an existing target. Keep `runtime.json` operator-managed;
choose guest-managed native configuration only when an in-guest wizard owns it.

Local `deployed` confirms infrastructure. `activating` records pending resource,
mesh and application readiness work; retry it with the original `deploy --resume`.
`active` is committed only after every replica reports application readiness. Inspect `confidential-agent report --json`
for instance attestation and daemon resource state, and `confidential-agent status --live` for
application readiness. A model request is a separate explicit check. No local
phase or readiness flag implies that a paid model request succeeded.

To expose the Hermes API through `service.connect`, set `API_SERVER_ENABLED=true`,
`API_SERVER_HOST=0.0.0.0`, and `API_SERVER_PORT=8642` in its resource configuration.
This lets the API accept TNG forwarding; keep its API key configured as in the
[Hermes example](../hermes-agent/README.md).

For [Qoder BYOK](https://docs.qoder.com/cli/custom-models), use the official
`/model` wizard and the provider catalog enabled for the account. A generic
OpenAI API key is not a Qoder account credential. Do not invent a BYOK
`settings.json` format. [OpenCode configuration](https://opencode.ai/docs/config/)
uses its native JSON schema. Set Qoder `general.enableAutoUpdate: false` and
OpenCode `autoupdate: false` for a fixed release image.

The installer reads fixed application versions and the Hermes commit from the
staged `runtime-contract.json`. Change that contract and rebuild to publish a
software update. Actual installed versions are recorded in
`/usr/local/share/confidential-agent/allinone-versions.txt`. Application pins do
not alone promise bit-for-bit reproducibility of every dependency.

The build removes npm and inactive Node download caches, and hard-links
identical application files only when their permissions, ownership and extended
attributes match. It retains Hermes frontend build dependencies for development
and rebuild commands. Hermes uses the official stripped Python archive from the
same pinned Python build; its download digest is still verified by Hermes PM.
The image records these selections in
`/usr/local/lib/cai-hermes/compact-python.json` and reclaimed file storage in
`/usr/local/share/confidential-agent/allinone-compaction.json`.

## Runtime modes and instance maintenance

Codex supports `mode: "cli"` and `mode: "app-server"`. For app-server, set
`port: 4500`, declare `service.ports: [4500]` and `service.connect: [4500]`, and
deliver a required `0600` token resource to
`/opt/data/allinone/home/.codex/app-server-token`. The selected allinone unit
starts the authenticated app-server; the independent Codex unit stays disabled.
Use `confidential-agent connect start --service SERVICE` with the native Codex remote client.

OpenClaw supports `mode: "gateway"` with `port: 18789`. Hermes supports gateway
mode; specifying `port: 8642` enables its HTTP API on `0.0.0.0`. Keep native API
authentication configured. A listening port must match `service.ports`; CLI mode
requires an empty port list. Unsupported modes and mismatches fail startup.

Different instances can retain the same native service port in one CA workspace.
Mesh client port collisions are mapped to independent loopback endpoints listed
by service ID in `/etc/cai/service-directory.json`; use that directory rather
than assuming a peer's local endpoint always equals its native port. Endpoint
assignments persist across topology changes and reboots within the same guest;
removed endpoints are reserved and never assigned to a different service.
Changing a local application port to a reserved client endpoint is rejected.
Every receiving guest, including client-only instances, must support stable
mesh endpoints before overlapping ports can be introduced. Legacy guests must
be rebuilt first. Local `confidential-agent connect` tunnels still need separate local
listening ports when kept running concurrently.

Background mesh and peering maintenance uses the saved deployment snapshot;
editing or moving the original AppSpec does not update an existing instance.
For resource delivery interrupted after infrastructure deployment completed,
restore the original input and run `confidential-agent deploy --resume --spec instance.yaml`.
Resume checks the original canonical AppSpec/resource paths, resource contents,
disk secret and instance identities, and delivers the recorded private resource
copies. Changing a referenced file while keeping the YAML unchanged is rejected.
Use explicit `confidential-agent inject` to apply current resources to an existing instance.
This keeps the recorded instance and identities. An interrupted infrastructure
operation has unknown results and is not replayed by `--resume`; inspect its
saved Shelter status before cleanup. `confidential-agent destroy` removes instance resources.
Reboot persistence does not imply data preservation across destroy/rebuild or
image replacement; those migrations are not implemented by this example.
Destroy leaves local identity material in the CA state directory; redeploying the
same service ID can reuse it. Use a new service ID for an independent identity.

## Build and publish

For publishing, CA uses AccessKey environment variables first, then the active
host Aliyun CLI profile (`aliyun configure`). AK and StsToken profiles are
supported. Select a named profile with `ALIBABA_CLOUD_PROFILE` / `ALICLOUD_PROFILE`,
or a custom CLI config file with `ALIBABA_CLOUD_CREDENTIALS_FILE` /
`ALICLOUD_SHARED_CREDENTIALS_FILE`. CA passes resolved credentials to both the
native cloud CLI and OSS uploader. No tools container or separate login is needed.
Role-based profiles must supply resolved temporary credentials through the
environment. Incomplete environment credentials are rejected rather than mixed
with another profile's keys.

```bash
confidential-agent build --spec examples/allinone/build.yaml
confidential-agent image publish allinone-builder --spec examples/allinone/build.yaml
test -f publisher.key || (umask 077; openssl genpkey -algorithm ED25519 -out publisher.key)
openssl pkey -in publisher.key -pubout -out publisher.pem
confidential-agent image export allinone-builder \
  --signing-key publisher.key --output image-release.json
```

Transfer `image-release.json` and
the independently trusted `publisher.pem` to the deployment host. Keep the
signing private key on the publisher. The deployment host needs CA and its
bundled Shelter RPM, cloud credentials, and instance resource files; it does
not need publisher state, build sources, or a local QCOW2.

For shared or marketplace images already available in a region, use `image export
allinone-builder --region REGION --image-id IMAGE_ID --signing-key publisher.key
--output image-release.json`. This signs metadata from the existing build without
uploading an image. Consumers can override `deploy.image.image_id` for their
region; runtime references stay unchanged. No deployment account UID is required.

Signatures are checked by default. `deploy --skip-signature-verification` explicitly
skips only the release signature and permits omitting the publisher key. It keeps
runtime reference/compatibility checks and attestation. `init --stage deploy`
accepts the same flag to record it in the generated spec. See the RPM walkthrough
for directory ownership and state retention.
