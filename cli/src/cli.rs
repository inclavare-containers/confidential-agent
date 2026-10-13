use clap::{Args, Parser, Subcommand, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;

fn default_state_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".confidential-agent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_state_dir_uses_home_when_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", "/tmp/ca-home");

        assert_eq!(
            default_state_dir(),
            PathBuf::from("/tmp/ca-home/.confidential-agent")
        );

        if let Some(value) = previous {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn default_state_dir_falls_back_to_root_without_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("HOME");
        std::env::remove_var("HOME");

        assert_eq!(
            default_state_dir(),
            PathBuf::from("/root/.confidential-agent")
        );

        if let Some(value) = previous {
            std::env::set_var("HOME", value);
        }
    }

    #[test]
    fn tui_args_have_safe_refresh_defaults_and_validate_ranges() {
        let cli = Cli::try_parse_from(["confidential-agent", "tui"]).unwrap();
        match cli.command {
            Commands::Tui(args) => {
                assert_eq!(args.service, None);
                assert_eq!(args.refresh, 2);
                assert_eq!(args.attestation_refresh, 60);
            }
            other => panic!("expected tui command, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "confidential-agent",
            "tui",
            "--service",
            "agent-a",
            "--refresh",
            "5",
            "--attestation-refresh",
            "0",
        ])
        .unwrap();
        match cli.command {
            Commands::Tui(args) => {
                assert_eq!(args.service.as_deref(), Some("agent-a"));
                assert_eq!(args.refresh, 5);
                assert_eq!(args.attestation_refresh, 0);
            }
            other => panic!("expected tui command, got {other:?}"),
        }

        assert!(Cli::try_parse_from(["confidential-agent", "tui", "--refresh", "0"]).is_err());
        assert!(Cli::try_parse_from([
            "confidential-agent",
            "tui",
            "--attestation-refresh",
            "3601"
        ])
        .is_err());
    }

    #[test]
    fn trustee_commands_parse_global_configuration_and_service_sync() {
        let configured = Cli::try_parse_from([
            "confidential-agent",
            "trustee",
            "configure",
            "--url",
            "https://trustee.example",
            "--admin-key",
            "/tmp/admin.key",
        ])
        .unwrap();
        match configured.command {
            Commands::Trustee(TrusteeArgs {
                command:
                    TrusteeCommands::Configure {
                        url,
                        management_url,
                        admin_key,
                        ca_cert,
                        force,
                    },
            }) => {
                assert_eq!(url, "https://trustee.example");
                assert_eq!(management_url, None);
                assert_eq!(admin_key, PathBuf::from("/tmp/admin.key"));
                assert_eq!(ca_cert, None);
                assert!(!force);
            }
            other => panic!("expected trustee configure command, got {other:?}"),
        }

        let sync = Cli::try_parse_from([
            "confidential-agent",
            "trustee",
            "sync",
            "--service",
            "agent-a",
        ])
        .unwrap();
        match sync.command {
            Commands::Trustee(TrusteeArgs {
                command: TrusteeCommands::Sync { service },
            }) => assert_eq!(service.as_deref(), Some("agent-a")),
            other => panic!("expected trustee sync command, got {other:?}"),
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "confidential-agent")]
#[command(about = "Confidential Agent host CLI")]
#[command(version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,

    #[arg(
        long,
        env = "CA_SHELTER_BIN",
        default_value = "shelter",
        global = true,
        hide = true
    )]
    pub(crate) shelter_bin: PathBuf,

    #[arg(long, default_value_os_t = default_state_dir(), global = true)]
    pub(crate) state_dir: PathBuf,

    #[arg(
        long,
        env = "CA_TOOLS_IMAGE",
        default_value = "confidential-agent-tools:latest",
        global = true
    )]
    pub(crate) tools_image: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    Init(InitArgs),
    Build(BuildArgs),
    Deploy(DeployArgs),
    Docs(DocsArgs),
    Spec(SpecArgs),
    Key(KeyArgs),
    Trustee(TrusteeArgs),
    #[command(hide = true)]
    Inject(InjectArgs),
    #[command(hide = true)]
    Mesh(MeshArgs),
    Connect(ConnectArgs),
    Peering(PeeringArgs),
    A2a(A2aArgs),
    Migrate(MigrateArgs),
    Image(ImageArgs),
    Ssh(SshArgs),
    Tui(TuiArgs),
    Status(StatusArgs),
    Report(ReportArgs),
    Destroy(DestroyArgs),
    Version,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub(crate) enum InitTarget {
    Openclaw,
    #[value(name = "openclaw-vllm")]
    OpenclawVllm,
    Hermes,
    Codex,
    #[value(name = "claudecode", alias = "claude-code")]
    Claudecode,
    #[value(name = "vllm-inference")]
    VllmInference,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub(crate) enum InitBuildBackend {
    Mkosi,
    #[value(name = "base-image")]
    BaseImage,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub(crate) enum InitReferenceValues {
    Sample,
    Rekor,
}

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    #[arg(value_enum)]
    pub(crate) target: Option<InitTarget>,
    #[arg(long, default_value = "./confidential-agent-init")]
    pub(crate) output_dir: PathBuf,
    #[arg(long)]
    pub(crate) force: bool,
    #[arg(long)]
    pub(crate) non_interactive: bool,
    #[arg(long)]
    pub(crate) region: Option<String>,
    #[arg(long)]
    pub(crate) zone_id: Option<String>,
    #[arg(long)]
    pub(crate) instance_type: Option<String>,
    #[arg(long)]
    pub(crate) disk_gb: Option<u32>,
    /// Number of replicas to deploy. For vllm-inference, replicas > 1 enables
    /// multi-node behind an ALB with OHTTP-encrypted connect (the slb_type and
    /// connect encryption are fixed for that scenario); replicas == 1 is a
    /// single-node deploy.
    #[arg(long, default_value_t = 1)]
    pub(crate) replicas: u32,
    #[arg(long, value_enum, default_value = "mkosi")]
    pub(crate) build_backend: InitBuildBackend,
    #[arg(long)]
    pub(crate) base_image: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub(crate) reference_values: Option<InitReferenceValues>,
    #[arg(long)]
    pub(crate) cosign_key: Option<PathBuf>,
    #[arg(long, default_value = "/usr/libexec/shelter/slsa/slsa-generator")]
    pub(crate) slsa_generator: PathBuf,
    #[arg(long, env = "DASHSCOPE_API_KEY")]
    pub(crate) dashscope_api_key: Option<String>,
    #[arg(
        long,
        default_value = "https://dashscope.aliyuncs.com/compatible-mode/v1"
    )]
    pub(crate) dashscope_base_url: String,
    #[arg(long, default_value = "https://dashscope.aliyuncs.com/apps/anthropic")]
    pub(crate) dashscope_anthropic_base_url: String,
    #[arg(long, env = "DASHSCOPE_MODEL")]
    pub(crate) model: Option<String>,
    #[arg(long)]
    pub(crate) gateway_token: Option<String>,
    #[arg(long)]
    pub(crate) disable_pep: bool,
    #[arg(long, default_value = "2026.5.7")]
    pub(crate) openclaw_version: String,
    #[arg(long, default_value = "22.19.0")]
    pub(crate) node_version: String,
    #[arg(long, default_value = "https://registry.npmmirror.com/")]
    pub(crate) npm_registry: String,
    #[arg(long, default_value = "Qwen/Qwen3.6-35B-A3B")]
    pub(crate) vllm_model_id: String,
    #[arg(long, default_value = "/opt/models/Qwen3.6-35B-A3B")]
    pub(crate) vllm_model_dir: String,
    #[arg(long, default_value = "Qwen3.6-35B-A3B")]
    pub(crate) vllm_served_model_name: String,
    #[arg(long, default_value_t = 8090)]
    pub(crate) vllm_port: u16,
    #[arg(long, default_value = "0.28.0")]
    pub(crate) vllm_version: String,
    #[arg(long, default_value = "release")]
    pub(crate) vllm_build_variants: String,
    #[arg(long)]
    pub(crate) model_key_file: Option<PathBuf>,
    #[arg(long)]
    pub(crate) oss_bucket: Option<String>,
    #[arg(long, default_value = "https://oss-cn-beijing-internal.aliyuncs.com")]
    pub(crate) oss_endpoint: String,
    #[arg(long)]
    pub(crate) enable_dingtalk: bool,
    #[arg(long, env = "DINGTALK_BOT_CLIENT_ID")]
    pub(crate) dingtalk_client_id: Option<String>,
    #[arg(long, env = "DINGTALK_BOT_CLIENT_SECRET")]
    pub(crate) dingtalk_client_secret: Option<String>,
    #[arg(long, default_value = "main")]
    pub(crate) hermes_branch: String,
    #[arg(long)]
    pub(crate) hermes_commit: Option<String>,
    #[arg(long)]
    pub(crate) hermes_api_server_key: Option<String>,
    #[arg(long)]
    pub(crate) codex_app_server_token: Option<String>,
    #[arg(long)]
    pub(crate) codex_version: Option<String>,
    #[arg(long)]
    pub(crate) claude_code_version: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct BuildArgs {
    #[arg(long, default_value = "confidential-agent.yaml")]
    pub(crate) spec: PathBuf,
    #[arg(long, hide = true)]
    pub(crate) render_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DeployArgs {
    #[arg(long, default_value = "confidential-agent.yaml")]
    pub(crate) spec: PathBuf,
    #[arg(long, hide = true)]
    pub(crate) skip_inject: bool,
    #[arg(long, hide = true)]
    pub(crate) render_only: bool,
    #[arg(long, env = "CA_SKIP_PEERING_CHECK")]
    pub(crate) skip_peering_check: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum OutputFormat {
    Markdown,
    Json,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum DocsTopic {
    Overview,
    Workflow,
    Appspec,
    Ops,
}

#[derive(Debug, Args)]
pub(crate) struct DocsArgs {
    #[arg(value_enum)]
    pub(crate) topic: DocsTopic,
    #[arg(long, value_enum, default_value = "markdown")]
    pub(crate) format: OutputFormat,
}

#[derive(Debug, Args)]
pub(crate) struct SpecArgs {
    #[command(subcommand)]
    pub(crate) command: SpecCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SpecCommands {
    Schema {
        #[arg(long, value_enum, default_value = "markdown")]
        format: OutputFormat,
    },
    Validate {
        #[arg(long, default_value = "confidential-agent.yaml")]
        spec: PathBuf,
        #[arg(long, value_enum, default_value = "markdown")]
        format: OutputFormat,
    },
}

#[derive(Debug, Args)]
pub(crate) struct KeyArgs {
    #[command(subcommand)]
    pub(crate) command: KeyCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum KeyCommands {
    #[command(name = "generate-cosign")]
    GenerateCosign {
        #[arg(long, default_value = "./cosign")]
        output_key_prefix: PathBuf,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct TrusteeArgs {
    #[command(subcommand)]
    pub(crate) command: TrusteeCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TrusteeCommands {
    Configure {
        #[arg(long, help = "Guest-visible KBS base URL (without /kbs/v0)")]
        url: String,
        #[arg(
            long,
            help = "CLI management base URL; defaults to --url (without /kbs/v0)"
        )]
        management_url: Option<String>,
        #[arg(long, help = "Ed25519 PKCS#8 PEM management private key")]
        admin_key: PathBuf,
        #[arg(long, help = "Optional PEM CA certificate for HTTPS")]
        ca_cert: Option<PathBuf>,
        #[arg(long, help = "Replace an existing configuration")]
        force: bool,
    },
    Show {
        #[arg(long)]
        json: bool,
    },
    Doctor {
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    Sync {
        #[arg(long)]
        service: Option<String>,
    },
    Adopt {
        #[arg(long)]
        attestation_policy_sha256: String,
        #[arg(long)]
        resource_policy_sha256: String,
    },
    Prune {
        #[arg(long, help = "Delete stale resources; default is a dry run")]
        apply: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct InjectArgs {
    #[arg(long)]
    pub(crate) spec: PathBuf,
    #[arg(long)]
    pub(crate) target_ip: String,
    #[arg(long, env = "CA_SKIP_PEERING_CHECK")]
    pub(crate) skip_peering_check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct MeshArgs {
    #[command(subcommand)]
    pub(crate) command: MeshCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum MeshCommands {
    Sync {
        #[arg(long)]
        service: Option<String>,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ConnectArgs {
    #[arg(long, hide = true)]
    pub(crate) render_only: bool,
    #[arg(long)]
    pub(crate) from_card: Option<String>,
    #[arg(long)]
    pub(crate) service: Option<String>,
    #[command(subcommand)]
    pub(crate) command: Option<ConnectCommands>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConnectCommands {
    Start(ConnectStartArgs),
    Stop(ConnectStopArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ConnectStartArgs {
    #[arg(long)]
    pub(crate) from_card: Option<String>,
    #[arg(long)]
    pub(crate) service: Option<String>,
    #[arg(long, default_value = "connect-ready.json")]
    pub(crate) ready_json: PathBuf,
    #[arg(long, default_value_t = 120)]
    pub(crate) wait_ready: u64,
    #[arg(long)]
    pub(crate) log_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ConnectStopArgs {
    #[arg(long, default_value = "connect-ready.json")]
    pub(crate) ready_json: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct PeeringArgs {
    #[command(subcommand)]
    pub(crate) command: PeeringCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PeeringCommands {
    Add {
        #[arg(long)]
        role: String,
        #[arg(long)]
        cidr: String,
        #[arg(long)]
        label: String,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        #[arg(long)]
        note: Option<String>,
    },
    List,
    Show {
        label: String,
    },
    Remove {
        label: String,
    },
    Apply {
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct A2aArgs {
    #[command(subcommand)]
    pub(crate) command: A2aCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum A2aCommands {
    Add {
        agent_card_url: String,
        #[arg(long)]
        alias: Option<String>,
        #[arg(long, value_delimiter = ',')]
        service: Vec<String>,
        #[arg(
            long,
            help = "Expected OIDC issuer for the peer AgentCard Sigstore keyless signature"
        )]
        signer_issuer: Option<String>,
        #[arg(
            long,
            help = "Expected certificate identity/subject for the peer AgentCard Sigstore keyless signature"
        )]
        signer_subject: Option<String>,
    },
    Remove {
        alias_or_url: String,
    },
    List,
    Show {
        alias_or_url: String,
    },
    Sync {
        #[arg(long)]
        alias: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct MigrateArgs {
    pub(crate) spec: PathBuf,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    #[arg(long)]
    pub(crate) peerings_out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ImageArgs {
    #[command(subcommand)]
    pub(crate) command: ImageCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ImageCommands {
    List {
        #[arg(long)]
        json: bool,
    },
    Rm {
        service: String,
        #[arg(long, short)]
        force: bool,
    },
    Publish(ImagePublishArgs),
    Unpublish(ImageUnpublishArgs),
    Prune(ImagePruneArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ImagePublishArgs {
    pub(crate) service: String,
    #[arg(long, default_value = "confidential-agent.yaml")]
    pub(crate) spec: PathBuf,
    #[arg(long, help = "Target region; defaults to deploy.region in the spec")]
    pub(crate) region: Option<String>,
    #[arg(long, help = "Image variant; defaults to deploy.image_variant")]
    pub(crate) variant: Option<String>,
    #[arg(long, help = "Do not wait for ECS image import completion")]
    pub(crate) no_wait: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ImageUnpublishArgs {
    pub(crate) service: String,
    #[arg(long)]
    pub(crate) region: Option<String>,
    #[arg(long)]
    pub(crate) variant: Option<String>,
    #[arg(long)]
    pub(crate) image_id: Option<String>,
    #[arg(long, short)]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ImagePruneArgs {
    #[arg(long)]
    pub(crate) dry_run: bool,
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct StatusArgs {
    #[arg(long)]
    pub(crate) service: Option<String>,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long, help = "Query read-only status from live guest daemons")]
    pub(crate) live: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TuiArgs {
    #[arg(long, help = "Start with a single local service")]
    pub(crate) service: Option<String>,
    #[arg(
        long,
        default_value_t = 2,
        value_parser = clap::value_parser!(u64).range(1..=60),
        help = "Guest daemon refresh interval in seconds"
    )]
    pub(crate) refresh: u64,
    #[arg(
        long,
        default_value_t = 60,
        value_parser = clap::value_parser!(u64).range(0..=3600),
        help = "Remote evidence verification interval in seconds; 0 disables automatic checks"
    )]
    pub(crate) attestation_refresh: u64,
}

#[derive(Debug, Args)]
pub(crate) struct SshArgs {
    pub(crate) service: String,
    #[arg(last = true)]
    pub(crate) ssh_args: Vec<OsString>,
}

#[derive(Debug, Args)]
pub(crate) struct ReportArgs {
    #[arg(long)]
    pub(crate) service: Option<String>,
    #[arg(long)]
    pub(crate) include_a2a: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct DestroyArgs {
    pub(crate) service: String,
}
