//! Pure state logic: classifying deployments, the aggregate light, and
//! deciding which notifications a new snapshot should produce.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Coarse phase of a deployment, derived from Laravel Cloud's `DeploymentStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Building,
    Succeeded,
    Failed,
    Cancelled,
    /// A status this version does not know: neutral, never polled fast.
    Unknown,
}

impl Phase {
    pub fn from_status(status: &str) -> Phase {
        match status {
            "deployment.succeeded" => Phase::Succeeded,
            "failed" | "build.failed" | "deployment.failed" => Phase::Failed,
            "cancelled" | "deployment.cancelled" | "build.cancelled" => Phase::Cancelled,
            // pending, build.*, deployment.{pending,created,queued,running}
            // (build.succeeded is an intermediate step, not a finished deploy)
            s if s == "pending" || s.starts_with("build.") || s.starts_with("deployment.") => {
                Phase::Building
            }
            _ => Phase::Unknown,
        }
    }

    pub fn color(self) -> crate::charts::Rgba {
        use crate::charts::{NEGATIVE, NEUTRAL, POSITIVE, WARNING};
        match self {
            Phase::Building => WARNING,
            Phase::Succeeded => POSITIVE,
            Phase::Failed => NEGATIVE,
            Phase::Cancelled | Phase::Unknown => NEUTRAL,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::Building => "Deploying",
            Phase::Succeeded => "Deployed",
            Phase::Failed => "Failed",
            Phase::Cancelled => "Cancelled",
            Phase::Unknown => "Unknown status",
        }
    }
}

/// The single traffic light shown in the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Green,
    Yellow,
    Red,
    Grey,
}

#[derive(Debug, Clone)]
pub struct Deployment {
    pub id: String,
    pub status: String,
    pub branch: String,
    pub commit_hash: String,
    pub commit_message: String,
    pub commit_author: Option<String>,
    pub failure_reason: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl Deployment {
    pub fn phase(&self) -> Phase {
        Phase::from_status(&self.status)
    }

    pub fn short_hash(&self) -> &str {
        self.commit_hash.get(..7).unwrap_or(&self.commit_hash)
    }

    /// Timestamp most relevant for "x ago": when it finished, else when it started.
    pub fn when(&self) -> Option<DateTime<Utc>> {
        self.finished_at.or(self.started_at)
    }
}

#[derive(Debug, Clone)]
pub struct Environment {
    pub id: String,
    pub name: String,
    pub status: String,
    pub domain: Option<String>,
    pub latest: Option<Deployment>,
    pub metrics: Option<Metrics>,
    /// Branches of recent deployments, newest first (offered when switching branch).
    pub recent_branches: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Application {
    /// Laravel Cloud application id.
    pub id: String,
    /// Local account (organization) id this app was loaded with.
    pub account: String,
    pub org: String,
    pub name: String,
    /// The repository's default branch, when Laravel Cloud reports one.
    pub default_branch: Option<String>,
    pub environments: Vec<Environment>,
}

impl Application {
    /// Branches to offer when switching an environment: the default branch, then
    /// branches any of the project's environments deployed recently.
    pub fn branch_choices(&self) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        let all = self.default_branch.iter().chain(
            self.environments
                .iter()
                .flat_map(|e| e.recent_branches.iter()),
        );
        for b in all {
            if !b.is_empty() && !out.contains(b) {
                out.push(b.clone());
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub apps: Vec<Application>,
    pub error: Option<String>,
    pub fetched_at: Option<DateTime<Utc>>,
    /// Environment ids hidden in settings: excluded from the light, menu and notifications.
    pub hidden: HashSet<String>,
    /// Spend per organization: (account id, org name, usage).
    pub usage: Vec<(String, String, crate::api::Usage)>,
    /// Errors per organization name.
    pub org_errors: Vec<(String, String)>,
}

impl Snapshot {
    /// Visible environments only.
    pub fn environments(&self) -> impl Iterator<Item = (&Application, &Environment)> {
        self.all_environments()
            .filter(|(_, e)| !self.hidden.contains(&e.id))
    }

    pub fn all_environments(&self) -> impl Iterator<Item = (&Application, &Environment)> {
        self.apps
            .iter()
            .flat_map(|a| a.environments.iter().map(move |e| (a, e)))
    }

    pub fn light(&self) -> Light {
        if self.error.is_some() && self.apps.is_empty() {
            return Light::Grey;
        }
        let phases: Vec<Phase> = self
            .environments()
            .filter_map(|(_, e)| e.latest.as_ref().map(Deployment::phase))
            .collect();
        if phases.contains(&Phase::Failed) {
            Light::Red
        } else if phases.contains(&Phase::Building) {
            Light::Yellow
        } else if phases.is_empty() {
            Light::Grey
        } else {
            Light::Green
        }
    }

    pub fn any_building(&self) -> bool {
        self.environments().any(|(_, e)| {
            e.latest
                .as_ref()
                .is_some_and(|d| d.phase() == Phase::Building)
        })
    }

    /// env id -> (deployment id, phase) for diffing between polls.
    pub fn index(&self) -> HashMap<String, (String, Phase)> {
        self.environments()
            .filter_map(|(_, e)| {
                e.latest
                    .as_ref()
                    .map(|d| (e.id.clone(), (d.id.clone(), d.phase())))
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub phase: Phase,
    pub title: String,
    pub body: String,
}

/// Compare the previous index with a new snapshot and produce notifications for
/// deployments that started or reached a terminal phase since the last poll.
/// Pass `None` as `prev` on the first poll to stay silent.
pub fn notices(prev: Option<&HashMap<String, (String, Phase)>>, next: &Snapshot) -> Vec<Notice> {
    let Some(prev) = prev else { return vec![] };
    let mut out = vec![];
    for (app, env) in next.environments() {
        let Some(dep) = &env.latest else { continue };
        let phase = dep.phase();
        let label = format!("{} · {}", app.name, env.name);
        // An environment missing from the last poll (new, or shown again after
        // being hidden) has no change to announce.
        let Some((prev_id, prev_phase)) = prev.get(&env.id) else {
            continue;
        };
        if *prev_id == dep.id && *prev_phase == phase {
            continue;
        }
        let commit = format!("{} {}", dep.short_hash(), first_line(&dep.commit_message));
        let notice = match phase {
            Phase::Building => {
                // Only announce a start for a deployment we have not seen before.
                if prev.get(&env.id).is_some_and(|(id, _)| *id == dep.id) {
                    continue;
                }
                Notice {
                    phase,
                    title: format!("Deploying {label}"),
                    body: commit,
                }
            }
            Phase::Succeeded => Notice {
                phase,
                title: format!("Deployed {label}"),
                body: commit,
            },
            Phase::Failed => Notice {
                phase,
                title: format!("Deploy failed: {label}"),
                body: dep.failure_reason.clone().unwrap_or(commit),
            },
            Phase::Cancelled => Notice {
                phase,
                title: format!("Deploy cancelled: {label}"),
                body: commit,
            },
            Phase::Unknown => continue,
        };
        out.push(notice);
    }
    out
}

// ---------------------------------------------------------------- metrics

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Period {
    #[default]
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "6h")]
    H6,
    #[serde(rename = "24h")]
    H24,
    #[serde(rename = "7d")]
    D7,
}

impl Period {
    pub fn as_str(self) -> &'static str {
        match self {
            Period::H1 => "1h",
            Period::H6 => "6h",
            Period::H24 => "24h",
            Period::D7 => "7d",
        }
    }
}

/// One line of a chart, e.g. one instance's CPU, or the `5xx` responses.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub label: String,
    pub points: Vec<f64>,
    /// For `http_response_count` this is the series' sum over the period.
    pub average: Option<f64>,
}

impl Series {
    pub fn last(&self) -> Option<f64> {
        self.points.last().copied()
    }
    pub fn peak(&self) -> Option<f64> {
        self.points.iter().copied().reduce(f64::max)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Metric {
    pub series: Vec<Series>,
    pub total: Option<f64>,
}

/// Each metric is `None` when the API reported it could not be read.
#[derive(Debug, Clone, PartialEq)]
pub struct Metrics {
    pub period: Period,
    pub cpu: Option<Metric>,
    pub memory: Option<Metric>,
    pub requests: Option<Metric>,
    pub replicas: Option<Metric>,
    pub workers: Option<Metric>,
}

impl Metrics {
    /// Highest current CPU across series, for the compact environment title.
    pub fn cpu_now(&self) -> Option<f64> {
        self.cpu
            .as_ref()?
            .series
            .iter()
            .filter_map(Series::last)
            .reduce(f64::max)
    }
}

pub fn fmt_pct(v: Option<f64>) -> String {
    match v {
        None => "–".into(),
        Some(v) if v < 10.0 => format!("{v:.1}%"),
        Some(v) => format!("{v:.0}%"),
    }
}

pub fn fmt_count(v: f64) -> String {
    match v {
        v if v >= 1_000_000.0 => format!("{:.1}M", v / 1_000_000.0),
        v if v >= 10_000.0 => format!("{:.0}k", v / 1000.0),
        v if v >= 1000.0 => format!("{:.1}k", v / 1000.0),
        v if v.fract() == 0.0 => format!("{v:.0}"),
        v => format!("{v:.1}"),
    }
}

pub fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

pub fn ago(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let s = (now - t).num_seconds().max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Word-wrap `s` into lines of at most `width` characters, at most `max_lines`
/// lines (the last one ends in `…` when text is cut).
pub fn wrap(s: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let len = cur.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
        while cur.chars().count() > width {
            let head: String = cur.chars().take(width).collect();
            cur = cur.chars().skip(width).collect();
            lines.push(head);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.pop().unwrap_or_default();
        lines.push(truncate(&format!("{last} …"), width));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_choices_start_with_default_and_have_no_duplicates() {
        let env = |branches: &[&str]| Environment {
            id: String::new(),
            name: String::new(),
            status: String::new(),
            domain: None,
            latest: None,
            metrics: None,
            recent_branches: branches.iter().map(|b| b.to_string()).collect(),
        };
        let app = Application {
            id: "a".into(),
            account: "x".into(),
            org: "Org".into(),
            name: "app".into(),
            default_branch: Some("main".into()),
            environments: vec![
                env(&["develop", "main"]),
                env(&["feature/x", "develop", ""]),
            ],
        };
        assert_eq!(app.branch_choices(), ["main", "develop", "feature/x"]);
    }

    fn dep(id: &str, status: &str) -> Deployment {
        Deployment {
            id: id.into(),
            status: status.into(),
            branch: "main".into(),
            commit_hash: "abcdef1234".into(),
            commit_message: "Fix thing\n\nlong body".into(),
            commit_author: None,
            failure_reason: None,
            started_at: None,
            finished_at: None,
        }
    }

    fn snap(deps: &[(&str, Option<Deployment>)]) -> Snapshot {
        Snapshot {
            apps: vec![Application {
                id: "app".into(),
                account: "a".into(),
                org: "Org".into(),
                name: "Shop".into(),
                default_branch: None,
                environments: deps
                    .iter()
                    .map(|(env, d)| Environment {
                        id: env.to_string(),
                        name: env.to_string(),
                        status: "running".into(),
                        domain: None,
                        latest: d.clone(),
                        metrics: None,
                        recent_branches: vec![],
                    })
                    .collect(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn classifies_statuses() {
        assert_eq!(Phase::from_status("deployment.succeeded"), Phase::Succeeded);
        assert_eq!(Phase::from_status("build.succeeded"), Phase::Building);
        assert_eq!(Phase::from_status("build.running"), Phase::Building);
        assert_eq!(Phase::from_status("pending"), Phase::Building);
        assert_eq!(Phase::from_status("build.failed"), Phase::Failed);
        assert_eq!(Phase::from_status("deployment.failed"), Phase::Failed);
        assert_eq!(Phase::from_status("failed"), Phase::Failed);
        assert_eq!(Phase::from_status("cancelled"), Phase::Cancelled);
        assert_eq!(Phase::from_status("something.new"), Phase::Unknown);
    }

    #[test]
    fn light_priority() {
        assert_eq!(snap(&[]).light(), Light::Grey);
        assert_eq!(snap(&[("prod", None)]).light(), Light::Grey);
        assert_eq!(
            snap(&[("prod", Some(dep("1", "deployment.succeeded")))]).light(),
            Light::Green
        );
        assert_eq!(
            snap(&[
                ("prod", Some(dep("1", "deployment.succeeded"))),
                ("stg", Some(dep("2", "build.running")))
            ])
            .light(),
            Light::Yellow
        );
        assert_eq!(
            snap(&[
                ("prod", Some(dep("1", "build.failed"))),
                ("stg", Some(dep("2", "build.running")))
            ])
            .light(),
            Light::Red
        );
    }

    #[test]
    fn cancelled_counts_as_settled() {
        assert_eq!(
            snap(&[("prod", Some(dep("1", "cancelled")))]).light(),
            Light::Green
        );
    }

    #[test]
    fn first_poll_is_silent() {
        let s = snap(&[("prod", Some(dep("1", "build.failed")))]);
        assert!(notices(None, &s).is_empty());
    }

    #[test]
    fn notifies_on_transitions() {
        let a = snap(&[("prod", Some(dep("1", "deployment.succeeded")))]);
        let b = snap(&[("prod", Some(dep("2", "build.running")))]);
        let c = snap(&[("prod", Some(dep("2", "deployment.running")))]);
        let d = snap(&[("prod", Some(dep("2", "deployment.succeeded")))]);

        let n = notices(Some(&a.index()), &b);
        assert_eq!(n.len(), 1);
        assert!(n[0].title.starts_with("Deploying Shop · prod"));
        assert_eq!(n[0].body, "abcdef1 Fix thing");

        // Still building in a later phase: no repeat notification.
        assert!(notices(Some(&b.index()), &c).is_empty());

        let n = notices(Some(&c.index()), &d);
        assert_eq!(n.len(), 1);
        assert!(n[0].title.starts_with("Deployed"));
    }

    #[test]
    fn quick_deploy_between_polls_still_notifies() {
        let a = snap(&[("prod", Some(dep("1", "deployment.succeeded")))]);
        let mut failed = dep("2", "build.failed");
        failed.failure_reason = Some("composer install failed".into());
        let b = snap(&[("prod", Some(failed))]);
        let n = notices(Some(&a.index()), &b);
        assert_eq!(n.len(), 1);
        assert!(n[0].title.starts_with("Deploy failed"));
        assert_eq!(n[0].body, "composer install failed");
    }

    #[test]
    fn unchanged_is_silent() {
        let a = snap(&[("prod", Some(dep("1", "deployment.succeeded")))]);
        assert!(notices(Some(&a.index()), &a).is_empty());
    }

    #[test]
    fn hidden_environments_do_not_count() {
        let mut s = snap(&[
            ("prod", Some(dep("1", "deployment.succeeded"))),
            ("stg", Some(dep("2", "build.failed"))),
        ]);
        assert_eq!(s.light(), Light::Red);
        s.hidden.insert("stg".into());
        assert_eq!(s.light(), Light::Green);
        assert_eq!(s.all_environments().count(), 2);
    }

    #[test]
    fn formats_numbers() {
        assert_eq!(fmt_pct(Some(3.149)), "3.1%");
        assert_eq!(fmt_pct(Some(42.4)), "42%");
        assert_eq!(fmt_pct(None), "–");
        assert_eq!(fmt_count(12.0), "12");
        assert_eq!(fmt_count(1234.0), "1.2k");
        assert_eq!(fmt_count(45_000.0), "45k");
        assert_eq!(fmt_count(2_500_000.0), "2.5M");
    }

    #[test]
    fn formats_ago_and_truncates() {
        let now = Utc::now();
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now - chrono::Duration::minutes(5), now), "5m ago");
        assert_eq!(ago(now - chrono::Duration::hours(3), now), "3h ago");
        assert_eq!(ago(now - chrono::Duration::days(2), now), "2d ago");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("hi", 5), "hi");
        assert_eq!(wrap("one two three", 7, 4), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4, 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a b c d", 1, 2), ["a", "…"]);
        assert!(wrap("  ", 10, 2).is_empty());
    }
}
