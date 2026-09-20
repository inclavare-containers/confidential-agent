use super::*;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::{Frame, Terminal};
use std::io::{self, IsTerminal, Stdout};
use std::sync::mpsc::{self, Receiver, Sender};

const ATTESTATION_POLICY: &str = "default";
const UI_POLL_INTERVAL: Duration = Duration::from_millis(100);
const STUCK_ATTESTATION_AFTER: Duration = Duration::from_secs(45);

#[derive(Debug, Clone)]
struct AgentSnapshot {
    local: LocalServiceState,
    daemon: Option<DaemonStatus>,
    live_error: Option<String>,
    trust: TrustProvenance,
}

impl AgentSnapshot {
    #[cfg(test)]
    fn local_only(local: LocalServiceState) -> Self {
        Self {
            local,
            daemon: None,
            live_error: None,
            trust: TrustProvenance::default(),
        }
    }

    fn is_deployed(&self) -> bool {
        matches!(self.local.phase.as_str(), "active" | "deployed")
    }

    fn attestation_target(&self) -> Option<AttestationTarget> {
        if !self.is_deployed() {
            return None;
        }
        let host = self.local.deploy.preferred_injection_ip()?;
        let tee = self.local.deploy.tee.trim();
        if tee.is_empty() {
            return None;
        }
        Some(AttestationTarget {
            service_id: self.local.service_id.clone(),
            host: host.to_string(),
            tee: tee.to_string(),
        })
    }
}

#[derive(Debug, Clone)]
struct AttestationTarget {
    service_id: String,
    host: String,
    tee: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TrustProvenance {
    reference_mode: String,
    rv_name: Option<String>,
    expected_uki: Option<String>,
    rekor_url: Option<String>,
    artifact_id: Option<String>,
    artifact_type: Option<String>,
    artifact_version: Option<String>,
    rekor_entry_uuid: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TrustVector {
    hardware: Option<i64>,
    executables: Option<i64>,
    configuration: Option<i64>,
    file_system: Option<i64>,
}

impl TrustVector {
    fn complete(self) -> bool {
        self.hardware.is_some()
            && self.executables.is_some()
            && self.configuration.is_some()
            && self.file_system.is_some()
    }

    fn rejected_dimensions(self) -> Vec<&'static str> {
        [
            ("hardware", self.hardware),
            ("executables", self.executables),
            ("configuration", self.configuration),
            ("file-system", self.file_system),
        ]
        .iter()
        .filter_map(|(name, value)| value.filter(|value| *value > 32).map(|_| *name))
        .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EvidenceSummary {
    mr_td_present: bool,
    rtmr_count: usize,
    uki_measurement: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum MeasurementComparison {
    #[default]
    NotChecked,
    Match,
    Mismatch,
    NoReference,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum AttestationVerdict {
    #[default]
    Unchecked,
    Checking,
    Verified,
    Rejected,
    Inconclusive,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct VerificationOutcome {
    verdict: AttestationVerdict,
    vector: TrustVector,
    evidence: EvidenceSummary,
    detail: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct VerificationCheck {
    outcome: VerificationOutcome,
    checked_at: Option<SystemTime>,
    checking_since: Option<Instant>,
}

#[derive(Debug)]
enum WorkerEvent {
    Refreshed(Result<Vec<AgentSnapshot>, String>),
    AttestationFinished {
        service_id: String,
        result: Result<VerificationOutcome, String>,
        checked_at: SystemTime,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum InputMode {
    #[default]
    Normal,
    Filter,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiAction {
    None,
    Refresh,
    VerifySelected,
    Quit,
}

struct TuiApp {
    agents: Vec<AgentSnapshot>,
    checks: BTreeMap<String, VerificationCheck>,
    selected: usize,
    filter: String,
    filter_before_input: String,
    input: String,
    input_mode: InputMode,
    show_help: bool,
    running: bool,
    refreshing: bool,
    status_message: String,
    last_refresh: Option<SystemTime>,
    last_refresh_started: Instant,
    live_refresh: Duration,
    attestation_refresh: Option<Duration>,
    service_scope: Option<String>,
    state_dir: PathBuf,
    verifier: AttestationVerifier,
}

impl TuiApp {
    fn new(
        state_dir: PathBuf,
        service_scope: Option<String>,
        agents: Vec<AgentSnapshot>,
        live_refresh: Duration,
        attestation_refresh: Option<Duration>,
        verifier: AttestationVerifier,
    ) -> Self {
        Self {
            agents,
            checks: BTreeMap::new(),
            selected: 0,
            filter: String::new(),
            filter_before_input: String::new(),
            input: String::new(),
            input_mode: InputMode::Normal,
            show_help: false,
            running: true,
            refreshing: false,
            status_message: "Starting live status collection…".to_string(),
            last_refresh: None,
            last_refresh_started: Instant::now()
                .checked_sub(live_refresh)
                .unwrap_or_else(Instant::now),
            live_refresh,
            attestation_refresh,
            service_scope,
            state_dir,
            verifier,
        }
    }

    fn visible_indices(&self) -> Vec<usize> {
        let query = self.filter.to_ascii_lowercase();
        self.agents
            .iter()
            .enumerate()
            .filter(|(_, agent)| {
                query.is_empty()
                    || agent.local.service_id.to_ascii_lowercase().contains(&query)
                    || agent.local.phase.to_ascii_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_agent(&self) -> Option<&AgentSnapshot> {
        let indices = self.visible_indices();
        indices
            .get(self.selected)
            .and_then(|index| self.agents.get(*index))
    }

    fn selected_service_id(&self) -> Option<String> {
        self.selected_agent()
            .map(|agent| agent.local.service_id.clone())
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_indices().len();
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    fn select_next(&mut self) {
        let len = self.visible_indices().len();
        if len > 0 {
            self.selected = (self.selected + 1).min(len - 1);
        }
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn select_first(&mut self) {
        self.selected = 0;
    }

    fn select_last(&mut self) {
        self.selected = self.visible_indices().len().saturating_sub(1);
    }

    fn replace_agents(&mut self, agents: Vec<AgentSnapshot>) {
        let selected_id = self.selected_service_id();
        self.agents = agents;
        let indices = self.visible_indices();
        self.selected = selected_id
            .as_deref()
            .and_then(|id| {
                indices.iter().position(|index| {
                    self.agents
                        .get(*index)
                        .map(|agent| agent.local.service_id == id)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(0);
        self.checks.retain(|service, _| {
            self.agents
                .iter()
                .any(|agent| agent.local.service_id == *service)
        });
        self.clamp_selection();
    }

    fn begin_refresh(&mut self, tx: &Sender<WorkerEvent>) {
        if self.refreshing {
            self.status_message = "Live refresh already in progress".to_string();
            return;
        }
        self.refreshing = true;
        self.last_refresh_started = Instant::now();
        self.status_message = "Refreshing guest daemon status…".to_string();
        let tx = tx.clone();
        let state_dir = self.state_dir.clone();
        let service_scope = self.service_scope.clone();
        thread::spawn(move || {
            let result = collect_tui_snapshots(&state_dir, service_scope.as_deref())
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(WorkerEvent::Refreshed(result));
        });
    }

    fn handle_worker_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Refreshed(Ok(agents)) => {
                let count = agents.len();
                self.replace_agents(agents);
                self.refreshing = false;
                self.last_refresh = Some(SystemTime::now());
                self.status_message = format!("Live status refreshed for {count} agent(s)");
            }
            WorkerEvent::Refreshed(Err(error)) => {
                self.refreshing = false;
                self.status_message = format!("Refresh failed: {}", single_line(&error));
            }
            WorkerEvent::AttestationFinished {
                service_id,
                result,
                checked_at,
            } => {
                let check = self.checks.entry(service_id.clone()).or_default();
                check.checking_since = None;
                check.checked_at = Some(checked_at);
                check.outcome = match result {
                    Ok(outcome) => outcome,
                    Err(error) => VerificationOutcome {
                        verdict: AttestationVerdict::Failed,
                        detail: Some(single_line(&error)),
                        ..VerificationOutcome::default()
                    },
                };
                self.status_message = format!(
                    "Remote evidence for {service_id}: {}",
                    verdict_label(check.outcome.verdict)
                );
            }
        }
    }

    fn start_attestation(&mut self, target: AttestationTarget, tx: &Sender<WorkerEvent>) {
        let check = self.checks.entry(target.service_id.clone()).or_default();
        if check.outcome.verdict == AttestationVerdict::Checking {
            self.status_message =
                format!("Evidence check already running for {}", target.service_id);
            return;
        }
        check.outcome.verdict = AttestationVerdict::Checking;
        check.outcome.detail = Some("Collecting and appraising fresh evidence".to_string());
        check.checking_since = Some(Instant::now());
        self.status_message = format!("Verifying remote evidence for {}…", target.service_id);

        let tx = tx.clone();
        let verifier = self.verifier.clone();
        thread::spawn(move || {
            let result = verifier
                .verify_remote_attestation_claims(&target.host, &target.tee)
                .map(|claims| evaluate_attestation_claims(claims.as_ref()))
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(WorkerEvent::AttestationFinished {
                service_id: target.service_id,
                result,
                checked_at: SystemTime::now(),
            });
        });
    }

    fn verify_selected(&mut self, tx: &Sender<WorkerEvent>) {
        match self
            .selected_agent()
            .and_then(AgentSnapshot::attestation_target)
        {
            Some(target) => self.start_attestation(target, tx),
            None => {
                self.status_message =
                    "Selected agent has no reachable TEE service in local state".to_string()
            }
        }
    }

    fn start_due_attestations(&mut self, tx: &Sender<WorkerEvent>) {
        let Some(interval) = self.attestation_refresh else {
            return;
        };
        let now = SystemTime::now();
        let targets = self
            .agents
            .iter()
            .filter_map(AgentSnapshot::attestation_target)
            .filter(|target| {
                let Some(check) = self.checks.get(&target.service_id) else {
                    return true;
                };
                if check.outcome.verdict == AttestationVerdict::Checking {
                    return false;
                }
                check
                    .checked_at
                    .and_then(|checked| now.duration_since(checked).ok())
                    .map(|age| age >= interval)
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        for target in targets {
            self.start_attestation(target, tx);
        }
    }

    fn expire_stuck_attestations(&mut self) {
        for (service, check) in &mut self.checks {
            if check.outcome.verdict != AttestationVerdict::Checking {
                continue;
            }
            if check
                .checking_since
                .map(|started| started.elapsed() >= STUCK_ATTESTATION_AFTER)
                .unwrap_or(false)
            {
                check.outcome = VerificationOutcome {
                    verdict: AttestationVerdict::Failed,
                    detail: Some(format!(
                        "verification did not finish within {}s",
                        STUCK_ATTESTATION_AFTER.as_secs()
                    )),
                    ..VerificationOutcome::default()
                };
                check.checking_since = None;
                check.checked_at = Some(SystemTime::now());
                self.status_message = format!("Remote evidence check timed out for {service}");
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> UiAction {
        if matches!(key.kind, KeyEventKind::Release) {
            return UiAction::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return UiAction::Quit;
        }
        if self.show_help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
            ) {
                self.show_help = false;
            }
            return UiAction::None;
        }

        match self.input_mode {
            InputMode::Normal => self.handle_normal_key(key),
            InputMode::Filter => self.handle_input_key(key, InputMode::Filter),
            InputMode::Command => self.handle_input_key(key, InputMode::Command),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> UiAction {
        match key.code {
            KeyCode::Char('q') => UiAction::Quit,
            KeyCode::Char('?') => {
                self.show_help = true;
                UiAction::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                UiAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_previous();
                UiAction::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.select_first();
                UiAction::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.select_last();
                UiAction::None
            }
            KeyCode::Char('r') => UiAction::Refresh,
            KeyCode::Char('v') | KeyCode::Enter => UiAction::VerifySelected,
            KeyCode::Char('/') => {
                self.input_mode = InputMode::Filter;
                self.filter_before_input = self.filter.clone();
                self.input = self.filter.clone();
                UiAction::None
            }
            KeyCode::Char(':') => {
                self.input_mode = InputMode::Command;
                self.input.clear();
                UiAction::None
            }
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    self.status_message = "Nothing to clear".to_string();
                } else {
                    self.filter.clear();
                    self.selected = 0;
                    self.status_message = "Filter cleared".to_string();
                }
                UiAction::None
            }
            _ => UiAction::None,
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent, mode: InputMode) -> UiAction {
        match key.code {
            KeyCode::Esc => {
                if mode == InputMode::Filter {
                    self.filter = self.filter_before_input.clone();
                    self.clamp_selection();
                }
                self.input_mode = InputMode::Normal;
                self.input.clear();
                UiAction::None
            }
            KeyCode::Enter => {
                let value = self.input.trim().to_string();
                self.input_mode = InputMode::Normal;
                self.input.clear();
                match mode {
                    InputMode::Filter => {
                        self.filter = value;
                        self.filter_before_input.clear();
                        self.selected = 0;
                        self.status_message = if self.filter.is_empty() {
                            "Filter cleared".to_string()
                        } else {
                            format!("Filter: {}", self.filter)
                        };
                        UiAction::None
                    }
                    InputMode::Command => self.execute_command(&value),
                    InputMode::Normal => UiAction::None,
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                if mode == InputMode::Filter {
                    self.filter = self.input.clone();
                    self.clamp_selection();
                }
                UiAction::None
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input.push(character);
                if mode == InputMode::Filter {
                    self.filter = self.input.clone();
                    self.clamp_selection();
                }
                UiAction::None
            }
            _ => UiAction::None,
        }
    }

    fn execute_command(&mut self, command: &str) -> UiAction {
        let mut parts = command.split_whitespace();
        match parts.next().unwrap_or("") {
            "q" | "quit" => UiAction::Quit,
            "r" | "refresh" => UiAction::Refresh,
            "v" | "verify" | "attest" => UiAction::VerifySelected,
            "help" | "?" => {
                self.show_help = true;
                UiAction::None
            }
            "agents" | "status" => {
                self.filter.clear();
                self.selected = 0;
                self.status_message = "Showing all agents".to_string();
                UiAction::None
            }
            "service" => {
                let Some(service) = parts.next() else {
                    self.status_message = "Usage: :service <id>".to_string();
                    return UiAction::None;
                };
                self.filter.clear();
                if let Some(index) = self
                    .agents
                    .iter()
                    .position(|agent| agent.local.service_id == service)
                {
                    self.selected = index;
                    self.status_message = format!("Selected {service}");
                } else {
                    self.status_message = format!("Unknown service: {service}");
                }
                UiAction::None
            }
            "" => UiAction::None,
            unknown => {
                self.status_message = format!("Unknown command: {unknown} (try :help)");
                UiAction::None
            }
        }
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error).context("failed to enter alternate terminal screen");
        }
        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = disable_raw_mode();
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
                return Err(error).context("failed to initialize terminal");
            }
        };
        let mut session = Self { terminal };
        session
            .terminal
            .clear()
            .context("failed to clear terminal")?;
        Ok(session)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

pub(super) fn cmd_tui(cli: &Cli, args: &TuiArgs) -> Result<()> {
    let initial = initial_tui_snapshots(&cli.state_dir, args.service.as_deref())?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "tui requires an interactive terminal; use 'status --live' for non-interactive output"
        );
    }

    let live_refresh = Duration::from_secs(args.refresh);
    let attestation_refresh =
        (args.attestation_refresh > 0).then_some(Duration::from_secs(args.attestation_refresh));
    let mut app = TuiApp::new(
        cli.state_dir.clone(),
        args.service.clone(),
        initial,
        live_refresh,
        attestation_refresh,
        AttestationVerifier::new(cli)?,
    );
    let (worker_tx, worker_rx) = mpsc::channel();
    let mut terminal = TerminalSession::enter()?;
    app.begin_refresh(&worker_tx);
    app.start_due_attestations(&worker_tx);

    while app.running {
        drain_worker_events(&mut app, &worker_rx);
        app.expire_stuck_attestations();
        if !app.refreshing && app.last_refresh_started.elapsed() >= app.live_refresh {
            app.begin_refresh(&worker_tx);
        }
        app.start_due_attestations(&worker_tx);

        terminal
            .terminal
            .draw(|frame| draw(frame, &mut app))
            .context("failed to draw tui")?;

        if event::poll(UI_POLL_INTERVAL).context("failed to poll terminal input")? {
            match event::read().context("failed to read terminal input")? {
                Event::Key(key) => match app.handle_key(key) {
                    UiAction::None => {}
                    UiAction::Refresh => app.begin_refresh(&worker_tx),
                    UiAction::VerifySelected => app.verify_selected(&worker_tx),
                    UiAction::Quit => app.running = false,
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

fn drain_worker_events(app: &mut TuiApp, rx: &Receiver<WorkerEvent>) {
    while let Ok(event) = rx.try_recv() {
        app.handle_worker_event(event);
    }
}

fn initial_tui_snapshots(
    state_dir: &Path,
    service_scope: Option<&str>,
) -> Result<Vec<AgentSnapshot>> {
    let mut states = read_service_states(state_dir)?;
    if let Some(service) = service_scope {
        states.retain(|state| state.service_id == service);
        if states.is_empty() {
            bail!("no local state for service '{service}'");
        }
    }
    Ok(states
        .into_iter()
        .map(|local| {
            let trust = load_trust_provenance(state_dir, &local);
            AgentSnapshot {
                local,
                daemon: None,
                live_error: None,
                trust,
            }
        })
        .collect())
}

fn collect_tui_snapshots(
    state_dir: &Path,
    service_scope: Option<&str>,
) -> Result<Vec<AgentSnapshot>> {
    let initial = initial_tui_snapshots(state_dir, service_scope)?;
    let snapshots = thread::scope(|scope| {
        initial
            .into_iter()
            .map(|snapshot| {
                let fallback = snapshot.clone();
                (
                    fallback,
                    scope.spawn(move || collect_one_snapshot(snapshot)),
                )
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(fallback, handle)| {
                handle.join().unwrap_or_else(|_| AgentSnapshot {
                    daemon: None,
                    live_error: Some("live status worker panicked".to_string()),
                    ..fallback
                })
            })
            .collect::<Vec<_>>()
    });
    Ok(snapshots
        .into_iter()
        .filter(|snapshot| !snapshot.local.service_id.is_empty())
        .collect())
}

fn collect_one_snapshot(snapshot: AgentSnapshot) -> AgentSnapshot {
    collect_one_snapshot_with(snapshot, |host| {
        fetch_daemon_status_from(host, DAEMON_STATUS_PORT, Duration::from_secs(3))
    })
}

fn collect_one_snapshot_with<F>(mut snapshot: AgentSnapshot, fetch: F) -> AgentSnapshot
where
    F: FnOnce(&str) -> Result<DaemonStatus>,
{
    if !snapshot.is_deployed() {
        snapshot.daemon = None;
        snapshot.live_error = None;
        return snapshot;
    }
    let Some(host) = snapshot
        .local
        .deploy
        .preferred_injection_ip()
        .map(str::to_string)
    else {
        snapshot.daemon = None;
        snapshot.live_error = Some("deployed service has no public or private IP".to_string());
        return snapshot;
    };
    match fetch(&host) {
        Ok(status) if status.service_id == snapshot.local.service_id => {
            snapshot.daemon = Some(status);
            snapshot.live_error = None;
        }
        Ok(status) => {
            snapshot.live_error = Some(format!(
                "daemon identity mismatch: expected '{}', received '{}'",
                snapshot.local.service_id, status.service_id
            ));
            snapshot.daemon = None;
        }
        Err(error) => {
            snapshot.daemon = None;
            snapshot.live_error = Some(single_line(&format!("{error:#}")));
        }
    }
    snapshot
}

fn load_trust_provenance(state_dir: &Path, state: &LocalServiceState) -> TrustProvenance {
    let mut trust = TrustProvenance {
        reference_mode: state.reference_values.clone(),
        rekor_entry_uuid: local_rekor_entry_uuid_for_state(state_dir, state),
        ..TrustProvenance::default()
    };

    if let Some(meta) = state.build.rekor_meta.as_deref().and_then(read_json_value) {
        trust.rv_name = json_string(&meta, "rv_name");
        trust.rekor_url = json_string(&meta, "rekor_url");
        trust.artifact_id = json_string(&meta, "artifact_id");
        trust.artifact_type = json_string(&meta, "artifact_type");
        trust.artifact_version = json_string(&meta, "artifact_version");
    }

    let rv_name = trust
        .rv_name
        .as_deref()
        .unwrap_or("measurement.uki.SHA-384");
    trust.expected_uki = state
        .build
        .sample_rv
        .as_deref()
        .and_then(read_json_value)
        .as_ref()
        .and_then(|values| reference_value(values, rv_name));
    trust
}

fn read_json_value(path: &Path) -> Option<serde_json::Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn reference_value(values: &serde_json::Value, name: &str) -> Option<String> {
    let value = values.get(name)?;
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            value
                .as_array()
                .and_then(|values| values.iter().find_map(serde_json::Value::as_str))
                .map(str::to_string)
        })
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
fn panic_fallback_state() -> LocalServiceState {
    LocalServiceState {
        schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
        service_id: String::new(),
        generation: 0,
        phase: "error".to_string(),
        attestation_mode: "challenge".to_string(),
        spec: LocalSpecState {
            path: PathBuf::new(),
            sha256: String::new(),
        },
        build: LocalBuildState {
            build_id: String::new(),
            image_name: String::new(),
            variant: String::new(),
            image_path: PathBuf::new(),
            images_dir: PathBuf::new(),
            cache_dir: PathBuf::new(),
            debug_ssh: None,
            sample_rv: None,
            rekor_meta: None,
            remote: false,
            published: BTreeMap::new(),
        },
        deploy: LocalDeployState {
            provider: String::new(),
            run_id: String::new(),
            resource_name: String::new(),
            terraform_dir: None,
            image_source: None,
            image_import_name: None,
            bucket: None,
            instance_id: None,
            security_group_id: None,
            private_ip: None,
            public_ip: None,
            tee: String::new(),
            published_image_id: None,
            instance_ids: Vec::new(),
            instance_private_ips: Vec::new(),
            slb_vip: None,
        },
        service: LocalServiceNetwork {
            ports: Vec::new(),
            connect: Vec::new(),
            mcp_ports: Vec::new(),
        },
        gateway_identity: None,
        resources: BTreeMap::new(),
        mesh_generation: 0,
        reference_values: String::new(),
    }
}

fn evaluate_attestation_claims(claims: Option<&serde_json::Value>) -> VerificationOutcome {
    let Some(claims) = claims else {
        return VerificationOutcome {
            verdict: AttestationVerdict::Inconclusive,
            detail: Some("verifier returned no decodable EAR claims".to_string()),
            ..VerificationOutcome::default()
        };
    };
    let Some(cpu) = claims.get("submods").and_then(|value| value.get("cpu0")) else {
        return VerificationOutcome {
            verdict: AttestationVerdict::Inconclusive,
            detail: Some("EAR claims are missing submods.cpu0".to_string()),
            ..VerificationOutcome::default()
        };
    };
    let Some(vector_value) = cpu.get("ear.trustworthiness-vector") else {
        return VerificationOutcome {
            verdict: AttestationVerdict::Inconclusive,
            detail: Some("EAR claims are missing the trustworthiness vector".to_string()),
            ..VerificationOutcome::default()
        };
    };
    let vector = TrustVector {
        hardware: vector_value
            .get("hardware")
            .and_then(|value| value.as_i64()),
        executables: vector_value
            .get("executables")
            .and_then(|value| value.as_i64()),
        configuration: vector_value
            .get("configuration")
            .and_then(|value| value.as_i64()),
        file_system: vector_value
            .get("file-system")
            .and_then(|value| value.as_i64()),
    };
    let tdx = cpu
        .get("ear.veraison.annotated-evidence")
        .and_then(|value| value.get("tdx"));
    let quote_body = tdx
        .and_then(|value| value.get("quote"))
        .and_then(|value| value.get("body"));
    let mr_td_present = quote_body
        .and_then(|value| value.get("mr_td"))
        .map(non_empty_json_value)
        .unwrap_or(false);
    let rtmr_count = quote_body
        .and_then(|value| value.as_object())
        .map(|object| {
            object
                .iter()
                .filter(|(key, value)| key.starts_with("rtmr_") && non_empty_json_value(value))
                .count()
        })
        .unwrap_or(0);
    let evidence = EvidenceSummary {
        mr_td_present,
        rtmr_count,
        uki_measurement: extract_uki_measurement(tdx),
    };

    if !vector.complete() {
        return VerificationOutcome {
            verdict: AttestationVerdict::Inconclusive,
            vector,
            evidence,
            detail: Some("trustworthiness vector is incomplete".to_string()),
        };
    }
    let rejected = vector.rejected_dimensions();
    if !rejected.is_empty() {
        return VerificationOutcome {
            verdict: AttestationVerdict::Rejected,
            vector,
            evidence,
            detail: Some(format!("policy rejected: {}", rejected.join(", "))),
        };
    }
    if !evidence.mr_td_present || evidence.rtmr_count == 0 {
        return VerificationOutcome {
            verdict: AttestationVerdict::Inconclusive,
            vector,
            evidence,
            detail: Some("EAR lacks required MR_TD/RTMR measurements".to_string()),
        };
    }
    VerificationOutcome {
        verdict: AttestationVerdict::Verified,
        vector,
        evidence,
        detail: Some("fresh TEE evidence passed the default appraisal policy".to_string()),
    }
}

fn extract_uki_measurement(tdx: Option<&serde_json::Value>) -> Option<String> {
    let events = tdx?.get("uefi_event_logs")?.as_array()?;
    events.iter().find_map(|event| {
        let is_boot_application = event
            .get("type_name")
            .and_then(serde_json::Value::as_str)
            .map(|value| value == "EV_EFI_BOOT_SERVICES_APPLICATION")
            .unwrap_or(false);
        let is_uki = event
            .get("details")
            .and_then(|value| value.get("device_paths"))
            .and_then(serde_json::Value::as_array)
            .map(|paths| {
                paths.iter().any(|path| {
                    path.as_str()
                        .map(|path| path.to_ascii_lowercase().contains("bootx64.efi"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if !is_boot_application || !is_uki {
            return None;
        }
        event
            .get("digests")
            .and_then(serde_json::Value::as_array)
            .and_then(|digests| {
                digests.iter().find_map(|digest| {
                    let algorithm = digest.get("alg")?.as_str()?;
                    if !algorithm.eq_ignore_ascii_case("SHA-384") {
                        return None;
                    }
                    digest
                        .get("digest")?
                        .as_str()
                        .map(str::trim)
                        .filter(|value| {
                            value.len() == 96 && value.chars().all(|ch| ch.is_ascii_hexdigit())
                        })
                        .map(str::to_ascii_lowercase)
                })
            })
    })
}

fn compare_measurements(measured: Option<&str>, expected: Option<&str>) -> MeasurementComparison {
    let Some(measured) = measured.map(str::trim).filter(|value| !value.is_empty()) else {
        return MeasurementComparison::NotChecked;
    };
    let Some(expected) = expected.map(str::trim).filter(|value| !value.is_empty()) else {
        return MeasurementComparison::NoReference;
    };
    if measured.eq_ignore_ascii_case(expected) {
        MeasurementComparison::Match
    } else {
        MeasurementComparison::Mismatch
    }
}

fn non_empty_json_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(value) => !value.trim().is_empty(),
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
        _ => true,
    }
}

fn draw(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let area = frame.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    draw_title(frame, app, chunks[0]);
    draw_summary(frame, app, chunks[1]);
    draw_body(frame, app, chunks[2]);
    draw_key_bar(frame, chunks[3]);
    draw_status_bar(frame, app, chunks[4]);
    if app.show_help {
        draw_help(frame, area);
    }
}

fn draw_title(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let selected = app
        .selected_agent()
        .map(|agent| agent.local.service_id.as_str())
        .unwrap_or("no agent");
    let refresh = if app.refreshing { "refreshing" } else { "live" };
    let title = Line::from(vec![
        Span::styled(" >_ Confidential Agent ", accent_bold()),
        Span::styled(" TUI ", badge_style()),
        Span::styled("  /  ", muted()),
        Span::styled(
            selected,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", muted()),
        Span::styled(refresh, if app.refreshing { warning() } else { success() }),
        Span::styled(format!(" every {}s", app.live_refresh.as_secs()), muted()),
    ]);
    frame.render_widget(
        Paragraph::new(title).style(Style::default().bg(Color::Rgb(20, 24, 33))),
        area,
    );
}

fn draw_summary(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let visible = app.visible_indices();
    let live = visible
        .iter()
        .filter(|index| app.agents[**index].daemon.is_some())
        .count();
    let attestable = visible
        .iter()
        .filter(|index| app.agents[**index].attestation_target().is_some())
        .count();
    let verified = visible
        .iter()
        .filter(|index| {
            let agent = &app.agents[**index];
            app.checks
                .get(&agent.local.service_id)
                .map(|check| check.outcome.verdict == AttestationVerdict::Verified)
                .unwrap_or(false)
        })
        .count();
    let mesh_members = visible
        .iter()
        .filter(|index| app.agents[**index].is_deployed())
        .count();
    let mesh_ready = visible
        .iter()
        .filter(|index| {
            app.agents[**index]
                .daemon
                .as_ref()
                .map(|daemon| daemon.mesh_ready)
                .unwrap_or(false)
        })
        .count();
    let content = Line::from(vec![
        Span::styled(" Agents ", muted()),
        Span::styled(visible.len().to_string(), heading()),
        Span::styled("   Daemons ", muted()),
        Span::styled(
            format!("{live}/{}", visible.len()),
            status_ratio_style(live, visible.len()),
        ),
        Span::styled("   Evidence verified ", muted()),
        Span::styled(
            format!("{verified}/{attestable}"),
            status_ratio_style(verified, attestable),
        ),
        Span::styled("   Mesh ready ", muted()),
        Span::styled(
            format!("{mesh_ready}/{mesh_members}"),
            status_ratio_style(mesh_ready, mesh_members),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border())
        .title(Span::styled(" Overview ", heading()));
    frame.render_widget(Paragraph::new(content).block(block), area);
}

fn draw_body(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    if area.height < 13 {
        draw_services(frame, app, area);
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);
    draw_services(frame, app, chunks[0]);
    if area.width >= 96 {
        let details = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
            .split(chunks[1]);
        draw_attestation(frame, app, details[0]);
        draw_service_network(frame, app, details[1]);
    } else {
        let details = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
            .split(chunks[1]);
        draw_attestation(frame, app, details[0]);
        draw_service_network(frame, app, details[1]);
    }
}

fn draw_services(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let visible = app.visible_indices();
    let narrow = area.width < 82;
    let compact = area.width < 118;
    let header_cells = if narrow {
        vec!["AGENT", "DAEMON", "ADDRESS", "PORTS M/C"]
    } else if compact {
        vec![
            "AGENT",
            "PHASE",
            "DAEMON",
            "ADDRESS",
            "MESH PORTS",
            "CONNECT",
        ]
    } else {
        vec![
            "AGENT",
            "PHASE",
            "DAEMON",
            "EVIDENCE",
            "APP",
            "MESH",
            "ADDRESS",
            "MESH PORTS",
            "CONNECT",
        ]
    };
    let header = Row::new(
        header_cells
            .into_iter()
            .map(|value| Cell::from(Span::styled(value, muted()))),
    )
    .height(1)
    .bottom_margin(1);
    let rows = visible.iter().map(|index| {
        let agent = &app.agents[*index];
        let daemon = agent.daemon.as_ref();
        let check = app.checks.get(&agent.local.service_id);
        let mesh_ports = join_tui_ports(&confidential_ports(
            &agent.local.service.ports,
            &agent.local.service.connect,
        ));
        let connect_ports = join_tui_ports(&agent.local.service.connect.iter().map(|cp| cp.port()).collect::<Vec<_>>());
        let address = preferred_display_address(agent);
        let evidence_label = if agent.attestation_target().is_none() {
            "n/a"
        } else {
            verdict_label(
                check
                    .map(|check| check.outcome.verdict)
                    .unwrap_or(AttestationVerdict::Unchecked),
            )
        };
        let cells = if narrow {
            vec![
                Cell::from(agent.local.service_id.clone()),
                Cell::from(Span::styled(daemon_label(agent), daemon_style(agent))),
                Cell::from(address),
                Cell::from(format!("M:{mesh_ports} C:{connect_ports}")),
            ]
        } else if compact {
            vec![
                Cell::from(agent.local.service_id.clone()),
                Cell::from(Span::styled(
                    &agent.local.phase,
                    phase_style(&agent.local.phase),
                )),
                Cell::from(Span::styled(daemon_label(agent), daemon_style(agent))),
                Cell::from(address),
                Cell::from(mesh_ports),
                Cell::from(connect_ports),
            ]
        } else {
            vec![
                Cell::from(agent.local.service_id.clone()),
                Cell::from(Span::styled(
                    &agent.local.phase,
                    phase_style(&agent.local.phase),
                )),
                Cell::from(Span::styled(daemon_label(agent), daemon_style(agent))),
                Cell::from(Span::styled(
                    evidence_label,
                    attestation_style(
                        check
                            .map(|check| check.outcome.verdict)
                            .unwrap_or(AttestationVerdict::Unchecked),
                    ),
                )),
                Cell::from(Span::styled(
                    readiness_label(daemon.map(|daemon| daemon.app_ready)),
                    readiness_style(daemon.map(|daemon| daemon.app_ready)),
                )),
                Cell::from(Span::styled(
                    readiness_label(daemon.map(|daemon| daemon.mesh_ready)),
                    readiness_style(daemon.map(|daemon| daemon.mesh_ready)),
                )),
                Cell::from(address),
                Cell::from(mesh_ports),
                Cell::from(connect_ports),
            ]
        };
        Row::new(cells)
    });
    let widths = if narrow {
        vec![
            Constraint::Percentage(28),
            Constraint::Length(11),
            Constraint::Length(15),
            Constraint::Min(14),
        ]
    } else if compact {
        vec![
            Constraint::Percentage(24),
            Constraint::Length(9),
            Constraint::Length(11),
            Constraint::Length(15),
            Constraint::Length(12),
            Constraint::Min(10),
        ]
    } else {
        vec![
            Constraint::Percentage(16),
            Constraint::Length(9),
            Constraint::Length(11),
            Constraint::Length(12),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(15),
            Constraint::Length(12),
            Constraint::Min(10),
        ]
    };
    let title = if app.filter.is_empty() {
        format!(" Agents ({}) ", visible.len())
    } else {
        format!(" Agents ({}) · filter: {} ", visible.len(), app.filter)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focused_border())
        .padding(Padding::horizontal(1))
        .title(Span::styled(title, heading()));
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 44, 58))
                .add_modifier(Modifier::BOLD),
        );
    let mut state = TableState::default();
    if !visible.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(table, area, &mut state);
    if visible.is_empty() {
        draw_empty(frame, area, "No agents match the current state/filter");
    }
}

fn draw_attestation(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border())
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            " Remote Authentication · fresh evidence ",
            heading(),
        ));
    let Some(agent) = app.selected_agent() else {
        frame.render_widget(
            Paragraph::new("No agent selected")
                .block(block)
                .style(muted()),
            area,
        );
        return;
    };
    let target = agent.attestation_target();
    let check = app.checks.get(&agent.local.service_id);
    let outcome = check.map(|check| &check.outcome);
    let verdict = if target.is_none() {
        None
    } else {
        Some(outcome.map(|outcome| outcome.verdict).unwrap_or_default())
    };
    let measured_uki = outcome.and_then(|outcome| outcome.evidence.uki_measurement.as_deref());
    let expected_uki = agent.trust.expected_uki.as_deref();
    let comparison = compare_measurements(measured_uki, expected_uki);
    let mut lines = vec![Line::from(vec![
        Span::styled("Verdict       ", muted()),
        Span::styled(
            verdict.map(verdict_label).unwrap_or("NOT AVAILABLE"),
            verdict.map(attestation_style).unwrap_or_else(muted),
        ),
    ])];
    lines.push(Line::from(vec![
        Span::styled("TEE           ", muted()),
        Span::styled(
            target
                .as_ref()
                .map(|target| target.tee.to_ascii_uppercase())
                .unwrap_or_else(|| "-".to_string()),
            heading(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Policy        ", muted()),
        Span::raw(format!("{ATTESTATION_POLICY} · ")),
        Span::styled(
            verdict.map(policy_result_label).unwrap_or("NOT AVAILABLE"),
            verdict.map(attestation_style).unwrap_or_else(muted),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Checked       ", muted()),
        Span::raw(
            check
                .and_then(|check| check.checked_at)
                .map(format_system_age)
                .unwrap_or_else(|| "never".to_string()),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("UKI measured  ", muted()),
        Span::raw(measured_uki.unwrap_or("-")),
    ]));
    lines.push(Line::from(vec![
        Span::styled("UKI expected  ", muted()),
        Span::raw(expected_uki.unwrap_or("-")),
    ]));
    lines.push(Line::from(vec![
        Span::styled("UKI match     ", muted()),
        Span::styled(
            measurement_comparison_label(comparison),
            measurement_comparison_style(comparison),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Reference     ", muted()),
        Span::raw(if agent.trust.reference_mode.is_empty() {
            "-".to_string()
        } else if let Some(rv_name) = agent.trust.rv_name.as_deref() {
            format!("{} · {rv_name}", agent.trust.reference_mode)
        } else {
            agent.trust.reference_mode.clone()
        }),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Rekor         ", muted()),
        Span::raw(agent.trust.rekor_url.as_deref().unwrap_or("not configured")),
    ]));
    if agent.trust.artifact_id.is_some()
        || agent.trust.artifact_type.is_some()
        || agent.trust.artifact_version.is_some()
    {
        let artifact = [
            agent.trust.artifact_id.as_deref(),
            agent.trust.artifact_type.as_deref(),
            agent.trust.artifact_version.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        lines.push(Line::from(vec![
            Span::styled("Artifact      ", muted()),
            Span::raw(artifact),
        ]));
    }
    if let Some(entry) = agent.trust.rekor_entry_uuid.as_deref() {
        lines.push(Line::from(vec![
            Span::styled("Rekor entry   ", muted()),
            Span::raw(compact_identifier(entry)),
        ]));
    }
    if outcome.is_none() {
        if target.is_some() {
            lines.push(Line::from(Span::styled(
                "Press v to collect and verify evidence now.",
                muted(),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Only deployed agents with a reachable TEE service can be verified here.",
                muted(),
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_service_network(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border())
        .padding(Padding::horizontal(1))
        .title(Span::styled(" Service Network ", heading()));
    let Some(agent) = app.selected_agent() else {
        frame.render_widget(
            Paragraph::new("No agent selected")
                .block(block)
                .style(muted()),
            area,
        );
        return;
    };
    let mesh_ports = confidential_ports(&agent.local.service.ports, &agent.local.service.connect);
    let lines = vec![
        Line::from(vec![
            Span::styled("Public IP      ", muted()),
            Span::raw(optional_text(agent.local.deploy.public_ip.as_deref())),
        ]),
        Line::from(vec![
            Span::styled("Private IP     ", muted()),
            Span::raw(optional_text(agent.local.deploy.private_ip.as_deref())),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Service ports  ", muted()),
            Span::raw(join_tui_ports(&agent.local.service.ports)),
        ]),
        Line::from(vec![
            Span::styled("Connect ports  ", muted()),
            Span::styled(join_tui_ports(&agent.local.service.connect.iter().map(|cp| cp.port()).collect::<Vec<_>>()), accent_bold()),
        ]),
        Line::from(vec![
            Span::styled("Mesh ports     ", muted()),
            Span::raw(join_tui_ports(&mesh_ports)),
        ]),
        Line::from(vec![
            Span::styled("MCP ports      ", muted()),
            Span::raw(join_tui_ports(&agent.local.service.mcp_ports)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_key_bar(frame: &mut Frame<'_>, area: Rect) {
    let line = Line::from(vec![
        Span::styled(" [j/k]", key_style()),
        Span::raw(" Navigate  "),
        Span::styled("[r]", key_style()),
        Span::raw(" Refresh  "),
        Span::styled("[v/Enter]", key_style()),
        Span::raw(" Verify evidence  "),
        Span::styled("[/]", key_style()),
        Span::raw(" Filter  "),
        Span::styled("[:]", key_style()),
        Span::raw(" Command  "),
        Span::styled("[?]", key_style()),
        Span::raw(" Help  "),
        Span::styled("[q]", key_style()),
        Span::raw(" Quit"),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(Color::Gray)),
        area,
    );
}

fn draw_status_bar(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    match app.input_mode {
        InputMode::Normal => {
            let refresh_age = app
                .last_refresh
                .map(format_system_age)
                .unwrap_or_else(|| "never".to_string());
            let line = Line::from(vec![
                Span::styled(" ", muted()),
                Span::styled(&app.status_message, muted()),
                Span::styled(format!("  ·  last refresh {refresh_age}"), muted()),
            ]);
            frame.render_widget(Paragraph::new(line), area);
        }
        InputMode::Filter | InputMode::Command => {
            let prefix = if app.input_mode == InputMode::Filter {
                "/"
            } else {
                ":"
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(prefix, accent_bold()),
                    Span::raw(&app.input),
                ])),
                area,
            );
            let cursor_x = area
                .x
                .saturating_add(1)
                .saturating_add(app.input.chars().count() as u16)
                .min(area.right().saturating_sub(1));
            frame.set_cursor(cursor_x, area.y);
        }
    }
}

fn draw_help(frame: &mut Frame<'_>, area: Rect) {
    let popup = centered_rect(84, 84, area);
    frame.render_widget(Clear, popup);
    let lines = vec![
        Line::from(Span::styled("Navigation", accent_bold())),
        Line::from("  j/k or arrows   select agent       g/G   first/last"),
        Line::from("  r               refresh now       Esc   clear filter"),
        Line::from("  v or Enter      verify fresh TEE evidence for the selected agent"),
        Line::from("  /               filter agents     :     command mode"),
        Line::from("  q / Ctrl-C      quit               ?     close help"),
        Line::from(""),
        Line::from(Span::styled("Commands", accent_bold())),
        Line::from("  :refresh  :verify  :service <id>  :agents  :help  :quit"),
        Line::from(""),
        Line::from(Span::styled("Security semantics", accent_bold())),
        Line::from("  VERIFIED: fresh evidence passed the default appraisal policy."),
        Line::from("  REJECTED: evidence was appraised, but the default policy rejected it."),
        Line::from("  INCONCLUSIVE: verifier output lacked required claims or boot measurements."),
        Line::from("  UKI MATCH: BOOTX64.EFI SHA-384 in fresh evidence equals the configured reference value."),
        Line::from("  Rekor entry: local upload provenance; use report when an online Rekor lookup is required."),
        Line::from("  Mesh READY: same-domain TNG configuration is active; it is not a connection claim."),
        Line::from("  Connect ports: host/client remote-authenticated ingress."),
        Line::from("  Mesh ports: service ports excluding connect, for same trust-domain traffic."),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focused_border())
        .padding(Padding::new(2, 2, 1, 1))
        .title(Span::styled(
            " Help · remote authentication layers ",
            heading(),
        ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .alignment(Alignment::Left),
        popup,
    );
}

fn draw_empty(frame: &mut Frame<'_>, area: Rect, message: &str) {
    if area.width <= 4 || area.height <= 2 {
        return;
    }
    let inner = Rect {
        x: area.x + 2,
        y: area.y + area.height / 2,
        width: area.width.saturating_sub(4),
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .style(muted()),
        inner,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn daemon_label(agent: &AgentSnapshot) -> &str {
    match agent.daemon.as_ref() {
        Some(daemon) => daemon.phase.as_str(),
        None if agent.is_deployed() => "unreachable",
        None => "-",
    }
}

fn daemon_style(agent: &AgentSnapshot) -> Style {
    match agent.daemon.as_ref() {
        Some(daemon) if daemon.phase == "running" => success(),
        Some(_) => warning(),
        None if agent.is_deployed() => failure(),
        None => muted(),
    }
}

fn phase_style(phase: &str) -> Style {
    match phase {
        "active" => success(),
        "deployed" | "built" => warning(),
        "failed" => failure(),
        _ => muted(),
    }
}

fn readiness_label(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "ready",
        Some(false) => "pending",
        None => "-",
    }
}

fn readiness_style(value: Option<bool>) -> Style {
    match value {
        Some(true) => success(),
        Some(false) => warning(),
        None => muted(),
    }
}

fn verdict_label(verdict: AttestationVerdict) -> &'static str {
    match verdict {
        AttestationVerdict::Unchecked => "UNCHECKED",
        AttestationVerdict::Checking => "CHECKING",
        AttestationVerdict::Verified => "VERIFIED",
        AttestationVerdict::Rejected => "REJECTED",
        AttestationVerdict::Inconclusive => "INCONCLUSIVE",
        AttestationVerdict::Failed => "FAILED",
    }
}

fn attestation_style(verdict: AttestationVerdict) -> Style {
    match verdict {
        AttestationVerdict::Verified => success(),
        AttestationVerdict::Checking | AttestationVerdict::Unchecked => warning(),
        AttestationVerdict::Rejected | AttestationVerdict::Failed => failure(),
        AttestationVerdict::Inconclusive => Style::default().fg(Color::Magenta),
    }
}

fn policy_result_label(verdict: AttestationVerdict) -> &'static str {
    match verdict {
        AttestationVerdict::Unchecked => "NOT RUN",
        AttestationVerdict::Checking => "CHECKING",
        AttestationVerdict::Verified => "PASSED",
        AttestationVerdict::Rejected => "REJECTED",
        AttestationVerdict::Inconclusive => "INCONCLUSIVE",
        AttestationVerdict::Failed => "ERROR",
    }
}

fn measurement_comparison_label(comparison: MeasurementComparison) -> &'static str {
    match comparison {
        MeasurementComparison::NotChecked => "NOT CHECKED",
        MeasurementComparison::Match => "MATCH",
        MeasurementComparison::Mismatch => "MISMATCH",
        MeasurementComparison::NoReference => "NO REFERENCE",
    }
}

fn measurement_comparison_style(comparison: MeasurementComparison) -> Style {
    match comparison {
        MeasurementComparison::Match => success(),
        MeasurementComparison::Mismatch => failure(),
        MeasurementComparison::NotChecked | MeasurementComparison::NoReference => warning(),
    }
}

fn status_ratio_style(good: usize, total: usize) -> Style {
    if total == 0 {
        muted()
    } else if good == total {
        success()
    } else if good == 0 {
        failure()
    } else {
        warning()
    }
}

fn format_system_age(time: SystemTime) -> String {
    let age = SystemTime::now().duration_since(time).unwrap_or_default();
    human_duration(age)
}

fn human_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 5 {
        "just now".to_string()
    } else if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

fn join_tui_ports(ports: &[u16]) -> String {
    if ports.is_empty() {
        "-".to_string()
    } else {
        ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn optional_text(value: Option<&str>) -> &str {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("-")
}

fn preferred_display_address(agent: &AgentSnapshot) -> String {
    let public = optional_text(agent.local.deploy.public_ip.as_deref());
    if public != "-" {
        return public.to_string();
    }
    optional_text(agent.local.deploy.private_ip.as_deref()).to_string()
}

fn compact_identifier(value: &str) -> String {
    const PREFIX: usize = 16;
    const SUFFIX: usize = 12;
    if value.len() <= PREFIX + SUFFIX + 1 {
        return value.to_string();
    }
    format!("{}…{}", &value[..PREFIX], &value[value.len() - SUFFIX..])
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn accent_bold() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn badge_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn heading() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn success() -> Style {
    Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD)
}

fn warning() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

fn failure() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
}

fn border() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn focused_border() -> Style {
    Style::default().fg(Color::Cyan)
}

fn key_style() -> Style {
    Style::default().fg(Color::Cyan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use confidential_agent_core::schema::DAEMON_STATUS_SCHEMA_VERSION;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    const UKI_DIGEST: &str = "4cd754646e188f26f19700283caa44bc448f4e6923bd9c22bc01926625cf3d311ca94193e4ec3147479da8739985902e";
    const REKOR_ENTRY: &str =
        "108e9186e8c5677aeae13e1d562f2f125eb3a74ae115a42eeb25d0d8d4717dd1857d6cbc23dc1c92";

    fn state(service: &str, phase: &str) -> LocalServiceState {
        let mut state = panic_fallback_state();
        state.service_id = service.to_string();
        state.phase = phase.to_string();
        state.deploy.provider = "aliyun".to_string();
        state.deploy.tee = "tdx".to_string();
        state.deploy.public_ip = Some("192.0.2.10".to_string());
        state.deploy.private_ip = Some("10.0.0.10".to_string());
        state.service.ports = vec![8000, 8443];
        state.service.connect = vec![(confidential_agent_core::spec::ConnectPort::Plain(8000))];
        state.service.mcp_ports = vec![8443];
        state
    }

    fn daemon(service: &str) -> DaemonStatus {
        DaemonStatus {
            schema: DAEMON_STATUS_SCHEMA_VERSION.to_string(),
            service_id: service.to_string(),
            phase: "running".to_string(),
            bootstrap_generation: 2,
            mesh_generation: 3,
            applied_resources: BTreeMap::new(),
            mesh_fingerprint: Some("fingerprint".to_string()),
            app_ready: true,
            mesh_ready: true,
            debug_ssh_ready: false,
            a2a_peers: BTreeMap::new(),
            last_error: None,
        }
    }

    fn snapshot(service: &str, phase: &str, live: bool) -> AgentSnapshot {
        AgentSnapshot {
            local: state(service, phase),
            daemon: live.then(|| daemon(service)),
            live_error: (!live && matches!(phase, "active" | "deployed"))
                .then(|| "connection refused".to_string()),
            trust: TrustProvenance::default(),
        }
    }

    fn verified_claims() -> serde_json::Value {
        json!({
            "submods": {
                "cpu0": {
                    "ear.trustworthiness-vector": {
                        "hardware": 2,
                        "executables": 3,
                        "configuration": 2,
                        "file-system": 2
                    },
                    "ear.veraison.annotated-evidence": {
                        "tdx": {
                            "quote": {"body": {"mr_td": "abcd", "rtmr_0": "1234", "rtmr_1": "5678"}},
                            "uefi_event_logs": [{
                                "type_name": "EV_EFI_BOOT_SERVICES_APPLICATION",
                                "details": {"device_paths": ["File(\\\\EFI\\\\BOOT\\\\BOOTX64.EFI)"]},
                                "digests": [{"alg": "SHA-384", "digest": UKI_DIGEST}]
                            }]
                        }
                    }
                }
            }
        })
    }

    fn app(agents: Vec<AgentSnapshot>) -> TuiApp {
        TuiApp::new(
            PathBuf::from("/state"),
            None,
            agents,
            Duration::from_secs(2),
            Some(Duration::from_secs(60)),
            AttestationVerifier::for_tests("confidential-agent-tools:test", PathBuf::from("/tmp")),
        )
    }

    fn render_text(app: &mut TuiApp, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.get(x, y).symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn verified_claims_require_green_vector_and_measurements() {
        let outcome = evaluate_attestation_claims(Some(&verified_claims()));
        assert_eq!(outcome.verdict, AttestationVerdict::Verified);
        assert_eq!(outcome.vector.hardware, Some(2));
        assert!(outcome.evidence.mr_td_present);
        assert_eq!(outcome.evidence.rtmr_count, 2);
        assert_eq!(
            outcome.evidence.uki_measurement.as_deref(),
            Some(UKI_DIGEST)
        );
    }

    #[test]
    fn uki_measurement_requires_boot_application_path_and_sha384() {
        let mut claims = verified_claims();
        claims["submods"]["cpu0"]["ear.veraison.annotated-evidence"]["tdx"]["uefi_event_logs"][0]
            ["type_name"] = json!("EV_SEPARATOR");
        let outcome = evaluate_attestation_claims(Some(&claims));
        assert_eq!(outcome.verdict, AttestationVerdict::Verified);
        assert!(outcome.evidence.uki_measurement.is_none());

        let mut claims = verified_claims();
        claims["submods"]["cpu0"]["ear.veraison.annotated-evidence"]["tdx"]["uefi_event_logs"][0]
            ["digests"][0]["alg"] = json!("SHA-256");
        assert!(evaluate_attestation_claims(Some(&claims))
            .evidence
            .uki_measurement
            .is_none());
    }

    #[test]
    fn policy_rejection_is_distinct_from_collection_failure() {
        let mut claims = verified_claims();
        claims["submods"]["cpu0"]["ear.trustworthiness-vector"]["executables"] = json!(33);
        let outcome = evaluate_attestation_claims(Some(&claims));
        assert_eq!(outcome.verdict, AttestationVerdict::Rejected);
        assert!(outcome.detail.unwrap().contains("executables"));
    }

    #[test]
    fn incomplete_claims_never_report_verified() {
        assert_eq!(
            evaluate_attestation_claims(None).verdict,
            AttestationVerdict::Inconclusive
        );
        let claims = json!({"submods": {"cpu0": {}}});
        assert_eq!(
            evaluate_attestation_claims(Some(&claims)).verdict,
            AttestationVerdict::Inconclusive
        );
        let mut claims = verified_claims();
        claims["submods"]["cpu0"]["ear.veraison.annotated-evidence"]["tdx"]["quote"]["body"] =
            json!({});
        assert_eq!(
            evaluate_attestation_claims(Some(&claims)).verdict,
            AttestationVerdict::Inconclusive
        );
    }

    #[test]
    fn measurement_comparison_distinguishes_all_states() {
        assert_eq!(
            compare_measurements(Some(UKI_DIGEST), Some(&UKI_DIGEST.to_ascii_uppercase())),
            MeasurementComparison::Match
        );
        assert_eq!(
            compare_measurements(Some(UKI_DIGEST), Some("different")),
            MeasurementComparison::Mismatch
        );
        assert_eq!(
            compare_measurements(Some(UKI_DIGEST), None),
            MeasurementComparison::NoReference
        );
        assert_eq!(
            compare_measurements(None, Some(UKI_DIGEST)),
            MeasurementComparison::NotChecked
        );
    }

    #[test]
    fn trust_provenance_loads_uki_reference_and_local_rekor_entry() {
        let temp = tempfile::tempdir().unwrap();
        let sample_rv = temp.path().join("reference-values.json");
        let rekor_meta = temp.path().join("rekor-meta.json");
        let image_dir = temp.path().join("images").join("build-1");
        let upload_dir = image_dir.join("slsa-output");
        fs::create_dir_all(&upload_dir).unwrap();
        fs::write(
            &sample_rv,
            serde_json::to_vec(&json!({"measurement.uki.SHA-384": [UKI_DIGEST]})).unwrap(),
        )
        .unwrap();
        fs::write(
            &rekor_meta,
            serde_json::to_vec(&json!({
                "artifact_id": "alpha-disk",
                "artifact_type": "uki",
                "artifact_version": "20260714",
                "rekor_url": "https://rekor.sigstore.dev",
                "rv_name": "measurement.uki.SHA-384"
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            upload_dir.join("rekor-v1-upload.txt"),
            format!("Created entry at https://rekor.sigstore.dev/api/v1/log/entries/{REKOR_ENTRY}"),
        )
        .unwrap();

        let mut state = state("alpha", "active");
        state.reference_values = "rekor".to_string();
        state.build.build_id = "build-1".to_string();
        state.build.image_path = image_dir.join("alpha.raw");
        state.build.sample_rv = Some(sample_rv);
        state.build.rekor_meta = Some(rekor_meta);
        let trust = load_trust_provenance(temp.path(), &state);

        assert_eq!(trust.reference_mode, "rekor");
        assert_eq!(trust.expected_uki.as_deref(), Some(UKI_DIGEST));
        assert_eq!(
            trust.rekor_url.as_deref(),
            Some("https://rekor.sigstore.dev")
        );
        assert_eq!(trust.artifact_id.as_deref(), Some("alpha-disk"));
        assert_eq!(trust.artifact_type.as_deref(), Some("uki"));
        assert_eq!(trust.artifact_version.as_deref(), Some("20260714"));
        assert_eq!(trust.rekor_entry_uuid.as_deref(), Some(REKOR_ENTRY));
    }

    #[test]
    fn filtering_and_navigation_are_bounded() {
        let mut app = app(vec![
            snapshot("alpha", "active", true),
            snapshot("beta", "built", false),
            snapshot("gamma", "active", false),
        ]);
        app.filter = "a".to_string();
        app.select_last();
        assert_eq!(app.selected_service_id().as_deref(), Some("gamma"));
        app.select_next();
        assert_eq!(app.selected_service_id().as_deref(), Some("gamma"));
        app.filter = "built".to_string();
        app.clamp_selection();
        assert_eq!(app.selected_service_id().as_deref(), Some("beta"));
        app.filter = "missing".to_string();
        app.clamp_selection();
        assert!(app.selected_agent().is_none());
    }

    #[test]
    fn replacing_live_data_preserves_selected_service() {
        let mut app = app(vec![
            snapshot("alpha", "active", false),
            snapshot("beta", "active", false),
        ]);
        app.selected = 1;
        app.replace_agents(vec![
            snapshot("beta", "active", true),
            snapshot("alpha", "active", true),
        ]);
        assert_eq!(app.selected_service_id().as_deref(), Some("beta"));
        assert!(app.selected_agent().unwrap().daemon.is_some());
    }

    #[test]
    fn live_collection_distinguishes_ready_unreachable_and_identity_mismatch() {
        let ready =
            collect_one_snapshot_with(AgentSnapshot::local_only(state("alpha", "active")), |_| {
                Ok(daemon("alpha"))
            });
        assert!(ready.daemon.is_some());
        assert!(ready.live_error.is_none());

        let unreachable =
            collect_one_snapshot_with(AgentSnapshot::local_only(state("alpha", "active")), |_| {
                bail!("connection timed out")
            });
        assert!(unreachable.daemon.is_none());
        assert!(unreachable
            .live_error
            .as_deref()
            .unwrap()
            .contains("timed out"));

        let mismatch =
            collect_one_snapshot_with(AgentSnapshot::local_only(state("alpha", "active")), |_| {
                Ok(daemon("unexpected-agent"))
            });
        assert!(mismatch.daemon.is_none());
        assert!(mismatch
            .live_error
            .as_deref()
            .unwrap()
            .contains("identity mismatch"));
    }

    #[test]
    fn worker_failure_and_stuck_check_are_visible_failures() {
        let mut app = app(vec![snapshot("alpha", "active", true)]);
        app.handle_worker_event(WorkerEvent::AttestationFinished {
            service_id: "alpha".to_string(),
            result: Err("verifier executable missing".to_string()),
            checked_at: SystemTime::now(),
        });
        assert_eq!(
            app.checks["alpha"].outcome.verdict,
            AttestationVerdict::Failed
        );
        assert!(app.checks["alpha"]
            .outcome
            .detail
            .as_deref()
            .unwrap()
            .contains("executable missing"));

        app.checks.insert(
            "alpha".to_string(),
            VerificationCheck {
                outcome: VerificationOutcome {
                    verdict: AttestationVerdict::Checking,
                    ..VerificationOutcome::default()
                },
                checked_at: None,
                checking_since: Some(
                    Instant::now()
                        .checked_sub(STUCK_ATTESTATION_AFTER + Duration::from_secs(1))
                        .unwrap(),
                ),
            },
        );
        app.expire_stuck_attestations();
        assert_eq!(
            app.checks["alpha"].outcome.verdict,
            AttestationVerdict::Failed
        );
        assert!(app.checks["alpha"]
            .outcome
            .detail
            .as_deref()
            .unwrap()
            .contains("did not finish"));
    }

    #[test]
    fn command_mode_handles_known_and_unknown_commands() {
        let mut app = app(vec![snapshot("alpha", "active", true)]);
        assert_eq!(app.execute_command("refresh"), UiAction::Refresh);
        assert_eq!(app.execute_command("attest"), UiAction::VerifySelected);
        assert_eq!(app.execute_command("quit"), UiAction::Quit);
        assert_eq!(app.execute_command("wat"), UiAction::None);
        assert!(app.status_message.contains("Unknown command"));
    }

    #[test]
    fn cancelling_live_filter_restores_previous_query() {
        let mut app = app(vec![
            snapshot("alpha", "active", true),
            snapshot("beta", "active", true),
        ]);
        app.filter = "alpha".to_string();
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.filter, "alph");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.filter, "alpha");
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn agent_tables_keep_addresses_mesh_and_connect_ports_at_smaller_widths() {
        let mut compact = app(vec![snapshot("alpha", "active", true)]);
        let rendered = render_text(&mut compact, 110, 26);
        assert!(rendered.contains("ADDRESS"));
        assert!(rendered.contains("MESH PORTS"));
        assert!(rendered.contains("CONNECT"));
        assert!(rendered.contains("192.0.2.10"));
        assert!(rendered.contains("8443"));
        assert!(rendered.contains("8000"));

        let mut narrow = app(vec![snapshot("alpha", "active", true)]);
        let rendered = render_text(&mut narrow, 80, 26);
        assert!(rendered.contains("PORTS M/C"));
        assert!(rendered.contains("M:8443 C:8000"));
    }

    #[test]
    fn preferred_address_uses_public_then_private() {
        let mut agent = snapshot("alpha", "active", true);
        assert_eq!(preferred_display_address(&agent), "192.0.2.10");
        agent.local.deploy.public_ip = Some("  ".to_string());
        assert_eq!(preferred_display_address(&agent), "10.0.0.10");
        agent.local.deploy.private_ip = None;
        assert_eq!(preferred_display_address(&agent), "-");
    }

    #[test]
    fn wide_dashboard_renders_authentication_layers() {
        let mut agent = snapshot("alpha", "active", true);
        agent.trust = TrustProvenance {
            reference_mode: "rekor".to_string(),
            rv_name: Some("measurement.uki.SHA-384".to_string()),
            expected_uki: Some(UKI_DIGEST.to_string()),
            rekor_url: Some("https://rekor.sigstore.dev".to_string()),
            artifact_id: Some("alpha-disk".to_string()),
            artifact_type: Some("uki".to_string()),
            artifact_version: Some("20260714".to_string()),
            rekor_entry_uuid: Some(REKOR_ENTRY.to_string()),
        };
        let mut app = app(vec![agent, snapshot("mesh-worker", "active", true)]);
        app.checks.insert(
            "alpha".to_string(),
            VerificationCheck {
                outcome: evaluate_attestation_claims(Some(&verified_claims())),
                checked_at: Some(SystemTime::now()),
                checking_since: None,
            },
        );
        let rendered = render_text(&mut app, 140, 44);
        assert!(rendered.contains("Remote Authentication"));
        assert!(rendered.contains("VERIFIED"));
        assert!(rendered.contains("default · PASSED"));
        assert!(rendered.contains("UKI measured"));
        assert!(rendered.contains("UKI expected"));
        assert!(rendered.contains("UKI match"));
        assert!(rendered.contains("MATCH"));
        assert!(rendered.contains("rekor.sigstore.dev"));
        assert!(rendered.contains("alpha-disk · uki · 20260714"));
        assert!(rendered.contains("Service Network"));
        assert!(rendered.contains("mesh-worker"));
        assert!(rendered.contains("192.0.2.10"));
        assert!(rendered.contains("10.0.0.10"));
        assert!(rendered.contains("Service ports"));
        assert!(rendered.contains("Connect ports"));
        assert!(rendered.contains("Mesh ports"));
        assert!(rendered.contains("MCP ports"));
        assert!(rendered.contains("8000"));
        assert!(rendered.contains("8443"));
        assert!(!rendered.contains("Local Trust-domain Mesh"));
        assert!(!rendered.contains("host/client remote-authenticated ingress"));
        assert!(!rendered.contains("same trust-domain service traffic"));
        assert!(!rendered.contains("A2A"));
        assert!(!rendered.contains("Endpoint"));
        assert!(!rendered.contains("Trust vector"));
        assert!(!rendered.contains("TCB / collat."));
        assert!(!rendered.contains("Detail"));
    }

    #[test]
    fn narrow_and_empty_dashboards_render_without_panicking() {
        let mut narrow = app(vec![snapshot("alpha", "active", false)]);
        let rendered = render_text(&mut narrow, 62, 22);
        assert!(rendered.contains("Confidential Agent"));
        assert!(rendered.contains("unreachable"));

        let mut empty = app(Vec::new());
        let rendered = render_text(&mut empty, 80, 24);
        assert!(rendered.contains("No agents match"));
    }

    #[test]
    fn help_overlay_documents_uki_rekor_and_mesh_semantics() {
        let mut app = app(vec![snapshot("alpha", "active", true)]);
        app.show_help = true;
        let rendered = render_text(&mut app, 120, 34);
        assert!(rendered.contains("Security semantics"));
        assert!(rendered.contains("UKI MATCH"));
        assert!(rendered.contains("local upload provenance"));
        assert!(rendered.contains("not a connection claim"));
        assert!(!rendered.contains("A2A"));
    }

    #[test]
    fn non_deployed_agent_has_no_attestation_target() {
        let agent = snapshot("alpha", "built", false);
        assert!(agent.attestation_target().is_none());
    }

    #[test]
    fn private_only_deployment_can_be_observed_and_verified() {
        let mut agent = snapshot("alpha", "active", false);
        agent.local.deploy.public_ip = None;
        agent.local.deploy.private_ip = Some("10.0.0.8".to_string());
        assert_eq!(agent.attestation_target().unwrap().host, "10.0.0.8");

        let collected = collect_one_snapshot_with(agent, |host| {
            assert_eq!(host, "10.0.0.8");
            Ok(daemon("alpha"))
        });
        assert!(collected.daemon.is_some());
    }

    #[test]
    fn compact_identifier_preserves_short_values_and_both_ends() {
        assert_eq!(compact_identifier("short"), "short");
        let compact = compact_identifier(REKOR_ENTRY);
        assert!(compact.starts_with(&REKOR_ENTRY[..16]));
        assert!(compact.ends_with(&REKOR_ENTRY[REKOR_ENTRY.len() - 12..]));
    }
}
