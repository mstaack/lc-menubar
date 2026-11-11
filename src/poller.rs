//! Background polling thread: owns API polling for every connected organization
//! and pushes merged snapshots to the UI.

use crate::api::{Client, Usage};
use crate::model::{self, Application, Metrics, Phase, Snapshot};
use crate::settings::{Settings, Shared};
use crate::system;
use chrono::Utc;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const FAST: Duration = Duration::from_secs(10);
const TOPOLOGY_TTL: Duration = Duration::from_secs(5 * 60);
const METRICS_TTL: Duration = Duration::from_secs(60);
const USAGE_TTL: Duration = Duration::from_secs(10 * 60);

/// (account id, token)
pub type Credentials = Vec<(String, String)>;

#[derive(Debug)]
pub enum Command {
    /// Poll now. `true` also reloads apps/environments and usage.
    Refresh {
        topology: bool,
    },
    SetAccounts(Credentials),
    /// Settings changed: re-render, and refetch metrics if needed.
    SettingsChanged,
}

pub fn spawn(
    accounts: Credentials,
    settings: Shared,
    on_snapshot: impl Fn(Snapshot) + Send + 'static,
) -> mpsc::UnboundedSender<Command> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("poller".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(Poller::new(accounts, settings).run(rx, on_snapshot));
        })
        .expect("spawn poller");
    tx
}

struct Org {
    name: String,
    apps: Vec<Application>,
    at: Instant,
}

struct Poller {
    accounts: Credentials,
    settings: Shared,
    orgs: HashMap<String, Org>,
    usage: HashMap<String, (Usage, Instant)>,
    /// Metrics per (environment, range), so switching back to a range is instant.
    metrics: HashMap<(String, crate::model::Period), (Metrics, Instant)>,
    prev_index: Option<HashMap<String, (String, Phase)>>,
    last: Snapshot,
}

impl Poller {
    fn new(accounts: Credentials, settings: Shared) -> Self {
        Self {
            accounts,
            settings,
            orgs: HashMap::new(),
            usage: HashMap::new(),
            metrics: HashMap::new(),
            prev_index: None,
            last: Snapshot::default(),
        }
    }

    fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    async fn run(
        mut self,
        mut rx: mpsc::UnboundedReceiver<Command>,
        on_snapshot: impl Fn(Snapshot),
    ) {
        let mut poll_api = true;
        loop {
            let settings = self.settings();
            let wait = if self.accounts.is_empty() {
                on_snapshot(Snapshot {
                    error: Some(
                        "No organization connected. Open Settings to add an API token".into(),
                    ),
                    ..Default::default()
                });
                None
            } else {
                let mut snap = if poll_api {
                    let snap = self.poll(&settings).await;
                    self.notify(&snap, &settings);
                    snap
                } else {
                    self.last.clone()
                };
                snap.hidden = settings.hidden_in(&snap.apps);
                self.last = snap.clone();
                let normal = Duration::from_secs(settings.refresh_secs);
                let wait = if snap.error.is_some() || !snap.org_errors.is_empty() {
                    // Back off while the API fails (rate limits, outages).
                    normal.max(Duration::from_secs(120))
                } else if snap.any_building() {
                    FAST
                } else {
                    normal
                };
                on_snapshot(snap);
                Some(wait)
            };

            let cmd = match wait {
                Some(d) => tokio::select! {
                    _ = tokio::time::sleep(d) => None,
                    c = rx.recv() => c,
                },
                None => rx.recv().await,
            };

            poll_api = true;
            match cmd {
                None => {}
                Some(Command::Refresh { topology }) => {
                    if topology {
                        self.orgs.clear();
                        self.usage.clear();
                    }
                    self.metrics.clear();
                }
                Some(Command::SettingsChanged) => {
                    let s = self.settings();
                    let period_changed = self.last.all_environments().any(|(_, e)| {
                        e.metrics
                            .as_ref()
                            .is_some_and(|m| m.period != s.metrics_period)
                    });
                    if s.metrics_enabled && (period_changed || !self.has_metrics(&s)) {
                        // Fast path: show cached data for the new range right away,
                        // then fetch only metrics (no deployments, no topology).
                        self.publish_metrics(&s, &on_snapshot);
                        self.fetch_metrics(&s, false).await;
                        self.publish_metrics(&s, &on_snapshot);
                    }
                    poll_api = s.show_spend && self.usage.is_empty();
                }
                Some(Command::SetAccounts(accounts)) => {
                    self.accounts = accounts;
                    self.orgs.clear();
                    self.usage.clear();
                    self.metrics.clear();
                    self.prev_index = None;
                    self.last = Snapshot::default();
                }
            }
        }
    }

    fn has_metrics(&self, s: &Settings) -> bool {
        self.metrics.keys().any(|(_, p)| *p == s.metrics_period)
    }

    /// Fill each environment's metrics from the cache for the current range. When a
    /// range is not cached yet, keep what is shown (the UI marks it as loading).
    fn with_cached_metrics(&self, s: &Settings, mut snap: Snapshot) -> Snapshot {
        for env in snap.apps.iter_mut().flat_map(|a| a.environments.iter_mut()) {
            if let Some((m, _)) = self.metrics.get(&(env.id.clone(), s.metrics_period)) {
                env.metrics = Some(m.clone());
            }
        }
        snap
    }

    fn publish_metrics(&mut self, s: &Settings, on_snapshot: &impl Fn(Snapshot)) {
        let mut snap = self.with_cached_metrics(s, self.last.clone());
        snap.hidden = s.hidden_in(&snap.apps);
        self.last = snap.clone();
        on_snapshot(snap);
    }

    /// Fetch metrics for visible environments concurrently. `only_stale` skips
    /// entries younger than the TTL (normal polling); a range switch fetches all
    /// that are missing or stale.
    async fn fetch_metrics(&mut self, s: &Settings, _only_stale: bool) {
        let period = s.metrics_period;
        let clients: HashMap<String, Client> = self
            .accounts
            .iter()
            .filter_map(|(a, t)| Client::new(t.clone()).ok().map(|c| (a.clone(), c)))
            .collect();
        let mut jobs = tokio::task::JoinSet::new();
        for (app, env) in self.last.all_environments() {
            if s.is_hidden(app, env) {
                continue;
            }
            let fresh = self
                .metrics
                .get(&(env.id.clone(), period))
                .is_some_and(|(_, at)| at.elapsed() < METRICS_TTL);
            if fresh {
                continue;
            }
            if let Some(c) = clients.get(&app.account) {
                let (c, id) = (c.clone(), env.id.clone());
                jobs.spawn(async move { (id.clone(), c.metrics(&id, period).await) });
            }
        }
        while let Some(res) = jobs.join_next().await {
            match res {
                Ok((id, Ok(m))) => {
                    self.metrics.insert((id, period), (m, Instant::now()));
                }
                Ok((id, Err(e))) => eprintln!("metrics for {id}: {e:#}"),
                Err(e) => eprintln!("metrics task: {e}"),
            }
        }
    }

    fn notify(&mut self, snap: &Snapshot, settings: &Settings) {
        if snap.apps.is_empty() {
            return;
        }
        let mut visible = snap.clone();
        visible.hidden = settings.hidden_in(&snap.apps);
        for n in model::notices(self.prev_index.as_ref(), &visible) {
            let wanted = match n.phase {
                Phase::Building => settings.notify_started,
                Phase::Succeeded => settings.notify_succeeded,
                Phase::Failed => settings.notify_failed,
                Phase::Cancelled => settings.notify_cancelled,
                Phase::Unknown => false,
            };
            if wanted {
                tokio::task::spawn_blocking(move || system::notify(&n.title, &n.body));
            }
        }
        self.prev_index = Some(snap.index());
    }

    async fn poll(&mut self, settings: &Settings) -> Snapshot {
        let mut org_errors = vec![];
        let mut apps: Vec<Application> = vec![];
        let mut clients: HashMap<String, Client> = HashMap::new();

        for (account, token) in self.accounts.clone() {
            let client = match Client::new(token) {
                Ok(c) => c,
                Err(e) => {
                    org_errors.push((account.clone(), e.to_string()));
                    continue;
                }
            };
            let stale = self
                .orgs
                .get(&account)
                .is_none_or(|o| o.at.elapsed() > TOPOLOGY_TTL);
            if stale {
                let (name, topo) = tokio::join!(client.organization(), client.topology());
                match topo {
                    Ok(mut list) => {
                        let name = name.unwrap_or_else(|_| fallback_name(settings, &account));
                        for a in &mut list {
                            a.account = account.clone();
                            a.org = name.clone();
                        }
                        self.orgs.insert(
                            account.clone(),
                            Org {
                                name,
                                apps: list,
                                at: Instant::now(),
                            },
                        );
                    }
                    Err(e) => {
                        let name = self
                            .orgs
                            .get(&account)
                            .map(|o| o.name.clone())
                            .unwrap_or_else(|| fallback_name(settings, &account));
                        org_errors.push((name, format!("{e:#}")));
                    }
                }
            }
            if let Some(org) = self.orgs.get(&account) {
                apps.extend(org.apps.iter().cloned());
            }
            if settings.show_spend
                && self
                    .usage
                    .get(&account)
                    .is_none_or(|(_, at)| at.elapsed() > USAGE_TTL)
            {
                match client.usage().await {
                    Ok(u) => {
                        self.usage.insert(account.clone(), (u, Instant::now()));
                    }
                    Err(e) => eprintln!("usage for {account}: {e:#}"),
                }
            }
            clients.insert(account, client);
        }

        let previous: HashMap<String, model::Deployment> = self
            .last
            .all_environments()
            .filter_map(|(_, e)| e.latest.clone().map(|d| (e.id.clone(), d)))
            .collect();

        let visible: Vec<(String, Client)> = apps
            .iter()
            .flat_map(|a| a.environments.iter().map(move |e| (a, e)))
            .filter(|(a, e)| !settings.is_hidden(a, e))
            .filter_map(|(a, e)| clients.get(&a.account).map(|c| (e.id.clone(), c.clone())))
            .collect();

        let mut deploys = tokio::task::JoinSet::new();
        for (id, c) in &visible {
            let (c, id) = (c.clone(), id.clone());
            deploys.spawn(async move { (id.clone(), c.latest_deployment(&id).await) });
        }
        let mut metric_jobs = tokio::task::JoinSet::new();
        if settings.metrics_enabled {
            let period = settings.metrics_period;
            for (id, c) in &visible {
                let fresh = self
                    .metrics
                    .get(&(id.clone(), period))
                    .is_some_and(|(_, at)| at.elapsed() < METRICS_TTL);
                if !fresh {
                    let (c, id) = (c.clone(), id.clone());
                    metric_jobs.spawn(async move { (id.clone(), c.metrics(&id, period).await) });
                }
            }
        }

        let mut latest = HashMap::new();
        let mut error = None;
        while let Some(res) = deploys.join_next().await {
            match res {
                Ok((id, Ok(d))) => {
                    latest.insert(id, d);
                }
                Ok((id, Err(e))) => {
                    error.get_or_insert_with(|| format!("{e:#}"));
                    latest.insert(id.clone(), previous.get(&id).cloned());
                }
                Err(e) => {
                    error.get_or_insert_with(|| e.to_string());
                }
            }
        }
        while let Some(res) = metric_jobs.join_next().await {
            match res {
                Ok((id, Ok(m))) => {
                    self.metrics.insert((id, m.period), (m, Instant::now()));
                }
                // Metrics are best-effort (e.g. hibernating envs): log, keep old values.
                Ok((id, Err(e))) => eprintln!("metrics for {id}: {e:#}"),
                Err(e) => eprintln!("metrics task: {e}"),
            }
        }

        for env in apps.iter_mut().flat_map(|a| a.environments.iter_mut()) {
            env.latest = latest
                .remove(&env.id)
                .unwrap_or_else(|| previous.get(&env.id).cloned());
            env.metrics = settings
                .metrics_enabled
                .then(|| {
                    self.metrics
                        .get(&(env.id.clone(), settings.metrics_period))
                        .map(|(m, _)| m.clone())
                })
                .flatten();
        }

        let usage = if settings.show_spend {
            self.accounts
                .iter()
                .filter_map(|(account, _)| {
                    let (u, _) = self.usage.get(account)?;
                    let name = self
                        .orgs
                        .get(account)
                        .map(|o| o.name.clone())
                        .unwrap_or_else(|| fallback_name(settings, account));
                    Some((account.clone(), name, u.clone()))
                })
                .collect()
        } else {
            vec![]
        };

        if error.is_none()
            && let Some((org, e)) = org_errors.first()
        {
            error = Some(format!("{org}: {e}"));
        }

        Snapshot {
            apps,
            error,
            fetched_at: Some(Utc::now()),
            hidden: Default::default(),
            usage,
            org_errors,
        }
    }
}

fn fallback_name(settings: &Settings, account: &str) -> String {
    settings
        .accounts
        .iter()
        .find(|a| a.id == account)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "Laravel Cloud".into())
}
