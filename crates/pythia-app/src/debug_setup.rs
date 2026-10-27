//! The debugger setup window: finds out why the database can't connect to
//! the debugger, and fixes it or says how.
//!
//! It reads what the user has ([`setup::facts`]), tries every host the
//! database might reach this computer by ([`setup::probe`]) and looks at
//! the local firewall ([`crate::firewall`]). From that it offers what's
//! missing: a host that works, the grants and network ACL entry (run on
//! this connection, as a DBA, or copied for one), and a firewall rule, run
//! with administrator rights after asking.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use egui::{RichText, Ui};
use pythia_config::{ConnectionProfile, DbKind, Privilege, Settings};
use pythia_db::ConnectParams;
use pythia_debugger::setup::{self, Candidate, Facts, Grant, Ports, Probe, Problem, Scope};
use pythia_ui::icons::Icon;

use crate::firewall;

/// How long the database gets to connect to a host being tried.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Debugger ports suggested when a firewall rule needs fixed ones.
const SUGGESTED_PORTS: &str = "4000-4009";

/// Work on a thread of its own.
#[derive(Default)]
enum Job<T> {
    #[default]
    Idle,
    Running(Receiver<T>),
    Done(T),
}

impl<T: Send + 'static> Job<T> {
    fn spawn(ctx: &egui::Context, work: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
        Job::Running(rx)
    }

    /// Takes the result if it's there; true when it just arrived.
    fn poll(&mut self) -> bool {
        let Job::Running(rx) = self else {
            return false;
        };
        match rx.try_recv() {
            Ok(value) => {
                *self = Job::Done(value);
                true
            }
            Err(TryRecvError::Disconnected) => {
                *self = Job::Idle;
                false
            }
            Err(TryRecvError::Empty) => false,
        }
    }

    fn done(&self) -> Option<&T> {
        match self {
            Job::Done(value) => Some(value),
            _ => None,
        }
    }

    fn running(&self) -> bool {
        matches!(self, Job::Running(_))
    }
}

/// What the app should change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupAction {
    /// Use this as the *Debugger host*.
    SetHost(String),
    /// Use these *Debugger ports*.
    SetPorts(String),
}

/// Who runs the database fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RunAs {
    /// The debugging user, on their own connection.
    #[default]
    Me,
    /// A saved connection (a DBA's).
    Saved,
    /// A user and password typed here, on the same database.
    Other,
}

/// A host being tried.
struct HostCheck {
    candidate: Candidate,
    probe: Job<Probe>,
}

/// How the checks came out.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Checking,
    /// The host in use works.
    Works,
    /// Another host works.
    Other(String),
    /// None works.
    Fails,
}

#[derive(Default)]
pub struct DebugSetup {
    pub open: bool,
    /// The connection checked, by display name, and how to log in to it.
    connection: String,
    params: Option<ConnectParams>,
    /// Why the window opened: a debug run that didn't connect.
    reason: Option<String>,
    /// The *Debugger host* setting, and the host the debugger uses for it.
    host_setting: String,
    in_use: String,
    /// The *Debugger ports* setting, parsed, and the field's draft.
    ports_setting: String,
    ports: Option<Ports>,
    ports_draft: String,
    facts: Job<Result<(Facts, Vec<Candidate>), String>>,
    hosts: Vec<HostCheck>,
    /// A host typed to try as well.
    other_host: String,
    firewall: Job<firewall::State>,
    /// The database server's address, for a firewall rule only it passes
    /// (resolving it may take a while).
    db_address: Job<Option<String>>,
    exe: Option<PathBuf>,
    // The database fix.
    acl_host: String,
    scope: Option<Scope>,
    debug_any: bool,
    run_as: RunAs,
    dba_profile: String,
    dba_user: String,
    dba_password: String,
    dba_sysdba: bool,
    /// The stored password of a saved connection, read once.
    stored: Option<(String, Option<String>)>,
    fix: Job<Result<(), String>>,
    // The firewall fix.
    rule: String,
    rule_edited: bool,
    only_db: bool,
    rule_job: Job<Result<(), String>>,
    /// What the last fix did: whether it worked, and what to say.
    notice: Option<(bool, String)>,
}

impl DebugSetup {
    /// Opens the window for `connection` (an Oracle connection logging in
    /// with `params`) and starts checking.
    pub fn open(
        &mut self,
        ctx: &egui::Context,
        connection: String,
        params: ConnectParams,
        settings: &Settings,
        reason: Option<String>,
    ) {
        let connect_string = params.connect_string.clone();
        *self = DebugSetup {
            open: true,
            connection,
            params: Some(params),
            reason,
            db_address: Job::spawn(ctx, move || db_address(&connect_string)),
            exe: std::env::current_exe().ok(),
            ..DebugSetup::default()
        };
        self.settings_changed(settings);
        self.check(ctx);
    }

    /// The host and ports to use changed (here or in Settings).
    fn settings_changed(&mut self, settings: &Settings) {
        self.host_setting = settings.debug_host.trim().to_owned();
        self.in_use = Some(self.host_setting.clone())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(pythia_debugger::target::default_host);
        self.ports_setting = settings.debug_ports.clone();
        self.ports = setup::parse_ports(&settings.debug_ports).ok().flatten();
        self.ports_draft = settings.debug_ports.clone();
    }

    /// Starts all checks again.
    fn check(&mut self, ctx: &egui::Context) {
        let Some(params) = self.params.clone() else {
            return;
        };
        let host_setting = self.host_setting.clone();
        self.facts = Job::spawn(ctx, move || {
            let conn = pythia_db::open_connection(&params).map_err(|e| e.text())?;
            let facts = setup::facts(&conn).map_err(|e| e.text())?;
            let candidates = setup::candidates(&host_setting, facts.client_address.as_deref());
            Ok((facts, candidates))
        });
        self.hosts.clear();
        if let Some(exe) = self.exe.clone() {
            self.firewall = Job::spawn(ctx, move || firewall::detect(&exe));
        }
        if !self.rule_edited {
            self.rule.clear();
        }
    }

    /// Takes results in and starts what they make possible.
    fn poll(&mut self, ctx: &egui::Context, actions: &mut Vec<SetupAction>) {
        if self.facts.poll()
            && let Some(Ok((facts, candidates))) = self.facts.done()
        {
            let mut candidates = candidates.clone();
            // The host in use first, also when it's none of the guesses.
            match candidates
                .iter()
                .position(|c| c.host.eq_ignore_ascii_case(&self.in_use))
            {
                Some(at) => candidates[..=at].rotate_right(1),
                None => candidates.insert(
                    0,
                    Candidate {
                        host: self.in_use.clone(),
                        source: "Pythia's default",
                    },
                ),
            }
            self.hosts = candidates
                .into_iter()
                .map(|candidate| HostCheck {
                    candidate,
                    probe: Job::Idle,
                })
                .collect();
            // The ACL entry is for the host set, else for the address the
            // server sees: the likeliest to lead back here.
            if self.acl_host.is_empty() {
                self.acl_host = Some(self.host_setting.clone())
                    .filter(|h| !h.is_empty())
                    .or_else(|| facts.client_address.clone())
                    .unwrap_or_else(|| self.in_use.clone());
                self.run_as = if facts.acl_admin {
                    RunAs::Me
                } else {
                    RunAs::Saved
                };
            }
        }
        // Each host on a port of its own: all at once unless the ports are
        // too few.
        let parallel = match &self.ports {
            Some(ports) => ports.clone().count().max(1),
            None => usize::MAX,
        };
        let running = self.hosts.iter().filter(|h| h.probe.running()).count();
        let ports = self.ports.clone();
        if let Some(params) = &self.params {
            for host in self
                .hosts
                .iter_mut()
                .filter(|h| matches!(h.probe, Job::Idle))
                .take(parallel.saturating_sub(running))
            {
                let params = params.clone();
                let name = host.candidate.host.clone();
                let ports = ports.clone();
                host.probe = Job::spawn(ctx, move || {
                    setup::probe(&params, &name, ports.as_ref(), PROBE_TIMEOUT)
                });
            }
        }
        for host in &mut self.hosts {
            host.probe.poll();
        }
        self.firewall.poll();
        self.db_address.poll();
        if self.fix.poll()
            && let Some(result) = self.fix.done()
        {
            self.notice = Some(match result {
                Ok(()) => {
                    // The ACL matches the host string: use the one it names.
                    if !self.acl_host.eq_ignore_ascii_case(&self.in_use) {
                        actions.push(SetupAction::SetHost(self.acl_host.clone()));
                        self.host_setting = self.acl_host.clone();
                        self.in_use = self.acl_host.clone();
                    }
                    (true, "The statements ran.".to_owned())
                }
                Err(e) => (false, e.clone()),
            });
            if matches!(result, Ok(())) {
                self.check(ctx);
            }
        }
        if self.rule_job.poll()
            && let Some(result) = self.rule_job.done()
        {
            self.notice = Some(match result {
                Ok(()) => (true, "The firewall rule is in place.".to_owned()),
                Err(e) => (false, e.clone()),
            });
            if matches!(result, Ok(())) {
                self.rule_edited = false;
                self.check(ctx);
            }
        }
    }

    fn facts(&self) -> Option<&Facts> {
        match self.facts.done() {
            Some(Ok((facts, _))) => Some(facts),
            _ => None,
        }
    }

    fn probe_of(&self, host: &str) -> Option<&Probe> {
        self.hosts
            .iter()
            .find(|h| h.candidate.host.eq_ignore_ascii_case(host))
            .and_then(|h| h.probe.done())
    }

    fn verdict(&self) -> Verdict {
        if self.hosts.is_empty() {
            return if self.facts.running() {
                Verdict::Checking
            } else {
                Verdict::Fails
            };
        }
        if matches!(self.probe_of(&self.in_use), Some(Probe::Reached { .. })) {
            return Verdict::Works;
        }
        if let Some(h) = self
            .hosts
            .iter()
            .find(|h| matches!(h.probe.done(), Some(Probe::Reached { .. })))
        {
            return Verdict::Other(h.candidate.host.clone());
        }
        if self.hosts.iter().any(|h| h.probe.done().is_none()) {
            Verdict::Checking
        } else {
            Verdict::Fails
        }
    }

    /// The problems of the hosts tried.
    fn problems(&self) -> impl Iterator<Item = (&str, &Problem)> {
        self.hosts.iter().filter_map(|h| match h.probe.done() {
            Some(Probe::Failed { problem, .. }) => Some((h.candidate.host.as_str(), problem)),
            _ => None,
        })
    }

    /// The database fix the checks call for, if any.
    fn grant(&self) -> Option<Grant> {
        let facts = self.facts()?;
        let works = matches!(self.verdict(), Verdict::Works | Verdict::Other(_));
        // Oracle checks the privilege before the ACL: without it, a probe
        // doesn't tell, and only a readable ACL does.
        let acl_unknown = || {
            !facts
                .aces
                .as_ref()
                .is_some_and(|aces| setup::acl_allows(aces, &self.acl_host, &self.ace_ports()))
        };
        let ace_needed = facts.acl
            && !works
            && match self.probe_of(&self.acl_host) {
                Some(Probe::Failed {
                    problem: Problem::Acl,
                    ..
                }) => true,
                Some(Probe::Failed {
                    problem: Problem::NoPrivilege,
                    ..
                })
                | Some(Probe::NotRun(_))
                | None => acl_unknown(),
                Some(Probe::Failed { .. } | Probe::Reached { .. }) => false,
            };
        let scope = self.scope.unwrap_or(Scope::Host);
        let grant = Grant {
            user: facts.user.clone(),
            debug_connect: !facts.debug_connect,
            debug_any: self.debug_any && !facts.debug_any,
            ace: ace_needed
                .then(|| scope.pattern(&self.acl_host))
                .flatten()
                .map(|pattern| (pattern, self.ace_ports())),
        };
        (!grant.is_empty()).then_some(grant)
    }

    fn ace_ports(&self) -> Ports {
        self.ports.clone().unwrap_or(setup::ANY_PORT)
    }

    /// Whether the database got as far as this computer's network: some
    /// host passed the ACL and then wasn't answered or was refused.
    fn blocked_on_the_way(&self) -> bool {
        self.problems()
            .any(|(_, p)| matches!(p, Problem::Refused | Problem::TimedOut))
    }

    /// The window. `saved` are the saved connections, for running the
    /// database fix as a DBA.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        settings: &Settings,
        saved: &[ConnectionProfile],
    ) -> Vec<SetupAction> {
        let mut actions = Vec::new();
        if !self.open {
            return actions;
        }
        if self.host_setting != settings.debug_host.trim()
            || self.ports_setting != settings.debug_ports
        {
            self.settings_changed(settings);
            self.check(ctx);
        }
        self.poll(ctx, &mut actions);
        let mut open = true;
        let screen = ctx.content_rect();
        egui::Window::new("Debugger setup")
            .id(egui::Id::new("debug-setup-window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([760.0, 820.0])
            .max_height((screen.height() - 48.0).max(240.0))
            .max_width((screen.width() - 48.0).max(320.0))
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(screen.center())
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.contents(ui, saved, &mut actions));
            });
        self.open &= open;
        actions
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        saved: &[ConnectionProfile],
        actions: &mut Vec<SetupAction>,
    ) {
        ui.label(format!(
            "To debug, the database connects back to Pythia (DBMS_DEBUG_JDWP.CONNECT_TCP). \
             That takes a privilege, a network ACL entry for the host it connects to, and a way \
             through firewalls. Checking {}.",
            self.connection
        ));
        let verdict = self.verdict();
        // What went wrong before, until it's fixed.
        if let Some(reason) = self.reason.as_ref().filter(|_| verdict != Verdict::Works) {
            ui.add_space(4.0);
            status(ui, Icon::Warning, reason);
        }
        ui.add_space(8.0);
        match verdict {
            Verdict::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking…");
                });
            }
            Verdict::Works => status(
                ui,
                Icon::Success,
                &format!("Debugging works: the database connects to {}.", self.in_use),
            ),
            Verdict::Other(host) => {
                ui.horizontal_wrapped(|ui| {
                    ui.add(Icon::Warning.image(ui));
                    ui.label(format!(
                        "The database doesn't connect to {}, but it does to {host}.",
                        self.in_use
                    ));
                    if ui.button(format!("Use {host}")).clicked() {
                        actions.push(SetupAction::SetHost(host.clone()));
                    }
                });
            }
            Verdict::Fails => {
                let text = match self.facts.done() {
                    Some(Err(e)) => format!("Pythia couldn't check: {e}"),
                    _ => "The database can't connect to Pythia yet. What's missing is below."
                        .to_owned(),
                };
                status(ui, Icon::Error, &text);
            }
        }
        if let Some((ok, text)) = &self.notice {
            status(ui, if *ok { Icon::Info } else { Icon::Error }, text);
        }

        if let Some(facts) = self.facts().cloned() {
            ui.add_space(8.0);
            ui.heading("Privileges");
            privileges(ui, &facts);
        }
        if !self.hosts.is_empty() {
            ui.add_space(8.0);
            ui.heading("Hosts");
            self.hosts_ui(ui, actions);
        }
        // Once it's clear the host the ACL entry would be for fails.
        let decided = match self.verdict() {
            Verdict::Fails => true,
            Verdict::Checking => self.probe_of(&self.acl_host).is_some(),
            _ => false,
        };
        if let Some(grant) = self.grant().filter(|_| decided) {
            ui.add_space(8.0);
            ui.heading("Fix in the database");
            self.database_fix(ui, grant, saved);
        }
        ui.add_space(8.0);
        ui.heading("Firewall");
        self.firewall_ui(ui, actions);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let busy = self.facts.running() || self.hosts.iter().any(|h| h.probe.running());
            if ui
                .add_enabled(!busy, egui::Button::new("Check again"))
                .clicked()
            {
                self.notice = None;
                self.check(ui.ctx());
            }
        });
    }

    fn hosts_ui(&mut self, ui: &mut Ui, actions: &mut Vec<SetupAction>) {
        ui.label(
            RichText::new(
                "How the database might reach this computer, each tried for real. One that \
                 works is all it takes.",
            )
            .weak(),
        );
        let mut retry = None;
        egui::Grid::new("debug-setup-hosts")
            .striped(true)
            .num_columns(4)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for (i, host) in self.hosts.iter().enumerate() {
                    let in_use = host.candidate.host.eq_ignore_ascii_case(&self.in_use);
                    let name = RichText::new(&host.candidate.host).monospace();
                    ui.label(if in_use { name.strong() } else { name });
                    ui.label(
                        RichText::new(if in_use {
                            format!("{} (in use)", host.candidate.source)
                        } else {
                            host.candidate.source.to_owned()
                        })
                        .weak(),
                    );
                    match host.probe.done() {
                        None => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.weak("trying…");
                            });
                        }
                        Some(Probe::Reached { port, from, .. }) => {
                            ui.horizontal(|ui| {
                                ui.add(Icon::Success.image(ui));
                                ui.label(format!("connected (port {port}, from {from})"));
                            });
                        }
                        Some(Probe::Failed {
                            problem,
                            error,
                            port,
                        }) => {
                            ui.horizontal(|ui| {
                                ui.add(Icon::Error.image(ui));
                                ui.label(problem.label()).on_hover_text(format!(
                                    "{}\n\n{error}",
                                    problem.explain(&host.candidate.host, *port)
                                ));
                            });
                        }
                        Some(Probe::NotRun(e)) => {
                            ui.horizontal(|ui| {
                                ui.add(Icon::Warning.image(ui));
                                ui.label("not tried").on_hover_text(e);
                            });
                        }
                    }
                    ui.horizontal(|ui| {
                        if !in_use
                            && matches!(host.probe.done(), Some(Probe::Reached { .. }))
                            && ui.button("Use").clicked()
                        {
                            actions.push(SetupAction::SetHost(host.candidate.host.clone()));
                        }
                        if host.probe.done().is_some() && ui.small_button("Retry").clicked() {
                            retry = Some(i);
                        }
                    });
                    ui.end_row();
                }
            });
        if let Some(i) = retry {
            self.hosts[i].probe = Job::Idle;
        }
        ui.horizontal(|ui| {
            ui.label("Try another:");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.other_host)
                    .hint_text("host name or address")
                    .desired_width(220.0),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let host = self.other_host.trim().to_owned();
            if (ui.button("Try").clicked() || entered) && !host.is_empty() {
                if let Some(existing) = self
                    .hosts
                    .iter_mut()
                    .find(|h| h.candidate.host.eq_ignore_ascii_case(&host))
                {
                    existing.probe = Job::Idle;
                } else {
                    self.hosts.push(HostCheck {
                        candidate: Candidate {
                            host,
                            source: "typed here",
                        },
                        probe: Job::Idle,
                    });
                }
                self.other_host.clear();
            }
        });
        // What the failures say beyond the table.
        let unknown: Vec<&str> = self
            .problems()
            .filter(|(_, p)| **p == Problem::UnknownHost)
            .map(|(h, _)| h)
            .collect();
        if !unknown.is_empty() {
            status(
                ui,
                Icon::Info,
                &format!(
                    "The database server can't resolve {}: an address or a full name (with \
                     the domain) works where a short name doesn't.",
                    unknown.join(", ")
                ),
            );
        }
        if let Some(aces) = self.facts().and_then(|f| f.aces.as_ref()) {
            for (host, problem) in self.problems() {
                if *problem == Problem::Acl
                    && let Some(allowed) = setup::allowed_ports(aces, host)
                    && self.ports.as_ref() != Some(&allowed)
                {
                    let text = setup::ports_text(&allowed);
                    ui.horizontal_wrapped(|ui| {
                        ui.add(Icon::Info.image(ui));
                        ui.label(format!(
                            "The network ACL allows {host} on ports {text} only, and the \
                             debugger listens elsewhere."
                        ));
                        if ui.button(format!("Listen on {text}")).clicked() {
                            actions.push(SetupAction::SetPorts(text.clone()));
                        }
                    });
                }
            }
        }
    }

    fn database_fix(&mut self, ui: &mut Ui, grant: Grant, saved: &[ConnectionProfile]) {
        let Some(facts) = self.facts().cloned() else {
            return;
        };
        if grant.ace.is_some() {
            ui.label(format!(
                "A network ACL entry has to let the database connect to the debugger. Oracle \
                 matches it against the host Pythia passes, so it's for {} (the Debugger host \
                 becomes that).",
                self.acl_host
            ));
            ui.horizontal(|ui| {
                ui.label("For");
                egui::ComboBox::from_id_salt("debug-setup-acl-host")
                    .selected_text(&self.acl_host)
                    .show_ui(ui, |ui| {
                        for host in &self.hosts {
                            ui.selectable_value(
                                &mut self.acl_host,
                                host.candidate.host.clone(),
                                format!("{}  ({})", host.candidate.host, host.candidate.source),
                            );
                        }
                    });
                for scope in Scope::ALL {
                    if let Some(pattern) = scope.pattern(&self.acl_host) {
                        let label = match scope {
                            Scope::Host => "this host".to_owned(),
                            Scope::Any => "any host (*)".to_owned(),
                            _ => pattern,
                        };
                        let selected = self.scope.unwrap_or(Scope::Host) == scope;
                        if ui.radio(selected, label).clicked() {
                            self.scope = Some(scope);
                        }
                    }
                }
            });
            if self.scope == Some(Scope::Any) {
                status(
                    ui,
                    Icon::Warning,
                    "Any host lets the user's sessions connect anywhere for debugging.",
                );
            }
        }
        if !facts.debug_any {
            ui.checkbox(
                &mut self.debug_any,
                "Also grant DEBUG ANY PROCEDURE (to debug other schemas' code)",
            );
        }
        let statements = grant.statements();
        let script = setup::script(&statements);
        // Selectable, not editable.
        ui.add(
            egui::TextEdit::multiline(&mut script.as_str())
                .code_editor()
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
        if self.run_as == RunAs::Me && !facts.can_run(&grant) {
            self.run_as = RunAs::Saved;
        }
        ui.horizontal(|ui| {
            ui.label("Run as");
            ui.add_enabled_ui(facts.can_run(&grant), |ui| {
                ui.radio_value(&mut self.run_as, RunAs::Me, &facts.user)
                    .on_disabled_hover_text(
                        "Needs EXECUTE on DBMS_NETWORK_ACL_ADMIN (and GRANT ANY PRIVILEGE for \
                         the grants)",
                    );
            });
            let oracle: Vec<&ConnectionProfile> = saved
                .iter()
                .filter(|p| p.kind == DbKind::Oracle && p.display_name() != self.connection)
                .collect();
            if !oracle.is_empty() {
                ui.radio_value(&mut self.run_as, RunAs::Saved, "saved connection");
                if self.dba_profile.is_empty()
                    && let Some(dba) = oracle.iter().find(|p| looks_like_dba(p)).or(oracle.first())
                {
                    self.dba_profile = dba.display_name();
                }
                ui.add_enabled_ui(self.run_as == RunAs::Saved, |ui| {
                    egui::ComboBox::from_id_salt("debug-setup-dba")
                        .selected_text(&self.dba_profile)
                        .show_ui(ui, |ui| {
                            for p in &oracle {
                                ui.selectable_value(
                                    &mut self.dba_profile,
                                    p.display_name(),
                                    p.display_name(),
                                );
                            }
                        });
                });
            } else if self.run_as == RunAs::Saved {
                self.run_as = RunAs::Other;
            }
            ui.radio_value(&mut self.run_as, RunAs::Other, "user");
        });
        let saved_profile = saved.iter().find(|p| p.display_name() == self.dba_profile);
        let stored = match (&self.stored, saved_profile) {
            (Some((name, password)), _) if *name == self.dba_profile => password.clone(),
            (_, Some(p)) => {
                let password = crate::secrets::load(&p.display_name());
                self.stored = Some((self.dba_profile.clone(), password.clone()));
                password
            }
            (_, None) => None,
        };
        let needs_password = match self.run_as {
            RunAs::Me => false,
            RunAs::Saved => stored.is_none(),
            RunAs::Other => true,
        };
        if self.run_as == RunAs::Other || needs_password {
            ui.horizontal(|ui| {
                if self.run_as == RunAs::Other {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.dba_user)
                            .hint_text("user, e.g. system")
                            .desired_width(140.0),
                    );
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.dba_password)
                        .password(true)
                        .hint_text("password (not saved)")
                        .desired_width(160.0),
                );
                if self.run_as == RunAs::Other {
                    ui.checkbox(&mut self.dba_sysdba, "SYSDBA");
                }
            });
        }
        ui.horizontal(|ui| {
            let params = match self.run_as {
                RunAs::Me => self.params.clone(),
                RunAs::Saved => saved_profile.map(|p| ConnectParams {
                    kind: DbKind::Oracle,
                    username: p.username.clone(),
                    password: stored.clone().unwrap_or_else(|| self.dba_password.clone()),
                    connect_string: p.connect_string.clone(),
                    privilege: p.privilege,
                    init_sql: Vec::new(),
                    ssh: None,
                }),
                RunAs::Other => self.params.as_ref().map(|own| ConnectParams {
                    username: self.dba_user.trim().to_owned(),
                    password: self.dba_password.clone(),
                    privilege: if self.dba_sysdba {
                        Privilege::Sysdba
                    } else {
                        Privilege::Normal
                    },
                    init_sql: Vec::new(),
                    ..own.clone()
                }),
            }
            .filter(|p| !p.username.is_empty())
            .filter(|p| !needs_password || !p.password.is_empty());
            let busy = self.fix.running();
            if ui
                .add_enabled(
                    params.is_some() && !busy,
                    egui::Button::new("Run statements"),
                )
                .on_hover_text("Runs these statements now, as the user chosen")
                .clicked()
                && let Some(params) = params
            {
                let container = facts.container.clone();
                self.notice = None;
                self.fix = Job::spawn(ui.ctx(), move || {
                    setup::run_as(&params, container.as_deref(), &statements)
                });
            }
            if busy {
                ui.spinner();
            }
            if ui
                .button("Copy")
                .on_hover_text("For a DBA to run")
                .clicked()
            {
                ui.ctx().copy_text(format!(
                    "-- Let {} debug PL/SQL with Pythia (run as a DBA)\n{script}",
                    facts.user
                ));
            }
            ui.menu_button("Undo script", |ui| {
                ui.label(RichText::new("Takes these grants and the ACL entry back:").weak());
                let undo = setup::script(&grant.undo());
                ui.add(
                    egui::TextEdit::multiline(&mut undo.as_str())
                        .code_editor()
                        .desired_width(480.0),
                );
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(undo);
                    ui.close();
                }
            });
        });
    }

    fn firewall_ui(&mut self, ui: &mut Ui, actions: &mut Vec<SetupAction>) {
        let Some(state) = self.firewall.done().cloned() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Looking at this computer's firewall…");
            });
            return;
        };
        let blocks = state.allows() == Some(false);
        let icon = match state.allows() {
            Some(true) => Icon::Success,
            Some(false) => Icon::Error,
            None => Icon::Info,
        };
        status(ui, icon, &state.summary());
        if state.managed {
            status(
                ui,
                Icon::Warning,
                "Your organization manages this firewall: rules added here don't apply. Ask IT \
                 for an inbound rule for Pythia (TCP, the debugger ports).",
            );
        }
        for note in &state.notes {
            status(ui, Icon::Info, note);
        }
        if self.blocked_on_the_way() && !blocks {
            status(
                ui,
                Icon::Info,
                "The database passed the ACL but its connection didn't arrive. If this \
                 computer's firewall lets it in, something between them stops it: a network \
                 firewall, NAT or a VPN. The database server has to be able to open a connection \
                 to this computer.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Debugger ports");
            ui.add(
                egui::TextEdit::singleline(&mut self.ports_draft)
                    .hint_text("any free port")
                    .desired_width(110.0),
            );
            match setup::parse_ports(&self.ports_draft) {
                Ok(ports) => {
                    let changed = ports != self.ports;
                    if ui
                        .add_enabled(changed, egui::Button::new("Apply"))
                        .clicked()
                    {
                        actions.push(SetupAction::SetPorts(self.ports_draft.trim().to_owned()));
                    }
                }
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            }
        });
        let Some(firewall) = state.firewall else {
            return;
        };
        if !(blocks || self.blocked_on_the_way()) || state.managed {
            return;
        }
        let Some(exe) = self.exe.clone() else {
            return;
        };
        let db_address = self.db_address.done().cloned().flatten();
        let remote = db_address.as_ref().filter(|_| self.only_db);
        let generated = firewall::rule_command(
            &state,
            &exe,
            self.ports.as_ref(),
            remote.map(String::as_str),
        );
        let Some(generated) = generated else {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "{}'s rules are about ports: the debugger needs fixed ones first.",
                    firewall.name()
                ));
                if ui.button(format!("Use {SUGGESTED_PORTS}")).clicked() {
                    actions.push(SetupAction::SetPorts(SUGGESTED_PORTS.into()));
                }
            });
            return;
        };
        if !self.rule_edited {
            self.rule = generated;
        }
        ui.label(format!(
            "This lets the database connect through {}:",
            firewall.name()
        ));
        if let Some(address) = &db_address {
            let before = self.only_db;
            ui.checkbox(
                &mut self.only_db,
                format!("Only from the database server ({address})"),
            )
            .on_hover_text(
                "Safer, but a database that connects from another address (RAC, NAT) is then \
                 blocked",
            );
            if before != self.only_db {
                self.rule_edited = false;
            }
        }
        if ui
            .add(
                egui::TextEdit::multiline(&mut self.rule)
                    .code_editor()
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            self.rule_edited = true;
        }
        ui.horizontal(|ui| {
            let busy = self.rule_job.running();
            if state.elevate
                && ui
                    .add_enabled(!busy, egui::Button::new("Run as administrator"))
                    .on_hover_text("Asks for administrator rights, then runs the command above")
                    .clicked()
            {
                let command = self.rule.clone();
                self.notice = None;
                self.rule_job =
                    Job::spawn(ui.ctx(), move || firewall::run_elevated(firewall, &command));
            }
            if busy {
                ui.spinner();
            }
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(self.rule.clone());
            }
        });
    }
}

#[cfg(test)]
impl DebugSetup {
    /// Why it opened.
    pub fn test_reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// The hosts tried and how each went (None: still trying).
    pub fn test_hosts(&self) -> Vec<(String, Option<Probe>)> {
        self.hosts
            .iter()
            .map(|h| (h.candidate.host.clone(), h.probe.done().cloned()))
            .collect()
    }

    /// The database fix's statements, if one is offered.
    pub fn test_fix(&self) -> Option<Vec<String>> {
        self.grant().map(|g| g.statements())
    }

    /// Whether the checks are done (the facts may have failed).
    pub fn test_done(&self) -> bool {
        !self.facts.running()
            && self.verdict() != Verdict::Checking
            && self.firewall.done().is_some()
            && !self.fix.running()
    }

    /// What the last fix said.
    pub fn test_notice(&self) -> Option<&(bool, String)> {
        self.notice.as_ref()
    }

    /// Whether the host in use works.
    pub fn test_works(&self) -> bool {
        self.verdict() == Verdict::Works
    }

    /// Runs the database fix as `user` on the same database.
    pub fn test_run_as(&mut self, user: &str, password: &str) {
        self.run_as = RunAs::Other;
        self.dba_user = user.into();
        self.dba_password = password.into();
    }
}

/// What the user has, a line each.
fn privileges(ui: &mut Ui, facts: &Facts) {
    if facts.debug_connect {
        status(
            ui,
            Icon::Success,
            &format!("{} has DEBUG CONNECT SESSION.", facts.user),
        );
    } else {
        status(
            ui,
            Icon::Error,
            &format!(
                "{} lacks DEBUG CONNECT SESSION, which debugging needs.",
                facts.user
            ),
        );
    }
    if facts.debug_any {
        status(
            ui,
            Icon::Success,
            "DEBUG ANY PROCEDURE: other schemas' code can be debugged too.",
        );
    } else {
        status(
            ui,
            Icon::Info,
            "No DEBUG ANY PROCEDURE: only code of the user's own schema can be debugged.",
        );
    }
    if !facts.acl {
        status(
            ui,
            Icon::Info,
            "Before 12.1 there is no network ACL for debugging.",
        );
    } else if facts.aces.is_none() {
        status(
            ui,
            Icon::Info,
            "Pythia can't read the network ACL (DBA_HOST_ACES): the hosts below are tried \
             instead.",
        );
    }
}

fn status(ui: &mut Ui, icon: Icon, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.add(icon.image(ui));
        ui.label(text);
    });
}

/// A saved connection that is likely a DBA's.
fn looks_like_dba(p: &ConnectionProfile) -> bool {
    p.privilege != Privilege::Normal
        || ["system", "sys", "admin"].contains(&p.username.to_lowercase().as_str())
}

/// The database server's address from a connect string (`host:port/service`
/// or a descriptor's `HOST=`), if it resolves to one that isn't this
/// computer's.
fn db_address(connect_string: &str) -> Option<String> {
    use std::net::ToSocketAddrs;
    let text = connect_string.trim();
    let upper = text.to_uppercase();
    let host = if let Some(at) = upper.find("HOST") {
        let rest = text[at + 4..].trim_start().strip_prefix('=')?.trim_start();
        rest.split([')', ' ']).next()?.to_owned()
    } else {
        let rest = text.strip_prefix("//").unwrap_or(text);
        // A TNS alias has neither: there is no host to read.
        if !rest.contains([':', '/']) {
            return None;
        }
        rest.split([':', '/']).next()?.to_owned()
    };
    let address = (host.as_str(), 1521)
        .to_socket_addrs()
        .ok()?
        .map(|a| a.ip())
        .find(|ip| ip.is_ipv4())?;
    (!address.is_loopback()).then(|| address.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_addresses() {
        assert_eq!(
            db_address("10.1.2.3:1521/ORCL").as_deref(),
            Some("10.1.2.3")
        );
        assert_eq!(db_address("//10.1.2.3/ORCL").as_deref(), Some("10.1.2.3"));
        assert_eq!(
            db_address("(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST = 10.9.8.7)(PORT=1521)))")
                .as_deref(),
            Some("10.9.8.7")
        );
        // This computer, or no host in it.
        assert_eq!(db_address("localhost:1521/FREEPDB1"), None);
        assert_eq!(db_address("ORCL"), None);
    }

    #[test]
    fn dba_guess() {
        let profile = |user: &str, privilege| ConnectionProfile {
            username: user.into(),
            privilege,
            ..ConnectionProfile::default()
        };
        assert!(looks_like_dba(&profile("SYSTEM", Privilege::Normal)));
        assert!(looks_like_dba(&profile("anyone", Privilege::Sysdba)));
        assert!(!looks_like_dba(&profile("hr", Privilege::Normal)));
    }
}
