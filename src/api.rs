//! Minimal Laravel Cloud API client (JSON:API over https://cloud.laravel.com/api).

use crate::model::{Application, Deployment, Environment, Metric, Metrics, Period, Series};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;

const BASE: &str = "https://cloud.laravel.com/api";

#[derive(Deserialize)]
struct Page<T> {
    data: Vec<Resource<T>>,
    #[serde(default)]
    links: Option<Links>,
}

#[derive(Deserialize)]
struct Links {
    next: Option<String>,
}

#[derive(Deserialize)]
struct Resource<T> {
    id: String,
    attributes: T,
}

#[derive(Deserialize)]
struct AppAttrs {
    name: String,
    #[serde(default)]
    repository: Option<RepoAttrs>,
}

#[derive(Deserialize)]
struct RepoAttrs {
    #[serde(default)]
    default_branch: Option<String>,
}

#[derive(Deserialize)]
struct EnvAttrs {
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    vanity_domain: Option<String>,
}

#[derive(Deserialize)]
struct DeployAttrs {
    #[serde(default)]
    status: String,
    #[serde(default)]
    branch_name: String,
    #[serde(default)]
    commit_hash: String,
    #[serde(default)]
    commit_message: String,
    commit_author: Option<String>,
    failure_reason: Option<String>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
}

fn to_deployment(r: Resource<DeployAttrs>) -> Deployment {
    let a = r.attributes;
    Deployment {
        id: r.id,
        status: a.status,
        branch: a.branch_name,
        commit_hash: a.commit_hash,
        commit_message: a.commit_message,
        commit_author: a.commit_author,
        failure_reason: a.failure_reason,
        started_at: a.started_at,
        finished_at: a.finished_at,
    }
}

/// Pick the newest deployment from a list page. The API does not document its sort
/// order, so rank by start (or finish) time. A deployment that has not started is
/// the newest only while it is still on its way (pending, queued): one that was
/// rejected before starting must not outrank every later deployment. Ties keep the
/// API's order (earlier in the list wins).
fn newest(deps: Vec<Deployment>) -> Option<Deployment> {
    let n = deps.len();
    deps.into_iter()
        .enumerate()
        .max_by_key(|(i, d)| {
            let rank = match d.started_at.or(d.finished_at) {
                Some(t) => (1, t),
                None if d.phase() == crate::model::Phase::Building => (2, DateTime::<Utc>::MAX_UTC),
                None => (0, DateTime::<Utc>::MIN_UTC),
            };
            (rank, n - i)
        })
        .map(|(_, d)| d)
}

#[derive(Deserialize)]
struct MetricsResp {
    data: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct RawMetric {
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    data: Vec<RawPoint>,
    #[serde(default)]
    average: Vec<Option<f64>>,
    total: Option<f64>,
}

#[derive(Deserialize)]
struct RawPoint {
    #[serde(default)]
    y: Vec<Option<f64>>,
}

/// A metric is `{"error": true}` when Cloud could not read it.
fn parse_metric(v: Option<&serde_json::Value>) -> Option<Metric> {
    let v = v?;
    if v.get("error").is_some() {
        return None;
    }
    let raw: RawMetric = serde_json::from_value(v.clone()).ok()?;
    let n = raw
        .labels
        .len()
        .max(raw.data.iter().map(|p| p.y.len()).max().unwrap_or(0));
    let series = (0..n)
        .map(|i| Series {
            label: raw.labels.get(i).cloned().unwrap_or_default(),
            points: raw
                .data
                .iter()
                .filter_map(|p| p.y.get(i).copied().flatten())
                .collect(),
            average: raw.average.get(i).copied().flatten(),
        })
        .collect();
    Some(Metric {
        series,
        total: raw.total,
    })
}

fn parse_metrics(resp: MetricsResp, period: Period) -> Metrics {
    let d = &resp.data;
    Metrics {
        period,
        cpu: parse_metric(d.get("cpu_usage")),
        memory: parse_metric(d.get("memory_usage")),
        requests: parse_metric(d.get("http_response_count")),
        replicas: parse_metric(d.get("replica_count")),
        workers: parse_metric(d.get("web_workers_count")),
    }
}

/// Locate `environment_variables: [{key, value}]` in an environment response.
/// The OpenAPI schema for this field is malformed, so search the attributes
/// (and one level of nesting) rather than relying on an exact path.
fn find_variables(v: &serde_json::Value) -> Option<Vec<(String, String)>> {
    fn as_vars(v: &serde_json::Value) -> Option<Vec<(String, String)>> {
        v.as_array()?
            .iter()
            .map(|e| {
                Some((
                    e.get("key")?.as_str()?.to_string(),
                    match e.get("value")? {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    },
                ))
            })
            .collect()
    }
    let attrs = v.pointer("/data/attributes")?;
    if let Some(found) = attrs.get("environment_variables").and_then(as_vars) {
        return Some(found);
    }
    attrs
        .as_object()?
        .values()
        .find_map(|inner| inner.get("environment_variables").and_then(as_vars))
}

/// Prefer the API's `message` (plus first validation error) over raw JSON.
fn api_error(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return crate::model::truncate(body, 300);
    };
    let mut msg = v
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    if let Some(first) = v
        .get("errors")
        .and_then(|e| e.as_object())
        .and_then(|e| e.values().next())
        .and_then(|e| e.get(0))
        .and_then(|e| e.as_str())
        && !msg.contains(first)
    {
        msg = format!("{msg} {first}");
    }
    if msg.trim().is_empty() {
        crate::model::truncate(body, 300)
    } else {
        msg.trim().to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogQuery {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub currency: String,
    pub spend_cents: i64,
    pub bandwidth_pct: Option<i64>,
    pub credits: Option<(i64, i64)>,
    /// (threshold, remaining percentage)
    pub alert: Option<(i64, i64)>,
    pub applications: Vec<(String, i64)>,
}

fn parse_usage(v: &serde_json::Value) -> Option<Usage> {
    let i = |p: &str| v.pointer(p).and_then(|x| x.as_i64());
    let summary = v.pointer("/data/summary")?;
    let applications = v
        .pointer("/data/application_totals/applications")
        .and_then(|a| a.as_array())
        .map(|apps| {
            apps.iter()
                .filter_map(|a| {
                    let name = ["name", "application_name", "application"]
                        .iter()
                        .find_map(|k| a.get(*k).and_then(|n| n.as_str()))?;
                    let cents = ["total_cost_cents", "cost_cents"]
                        .iter()
                        .find_map(|k| a.get(*k).and_then(|n| n.as_i64()))?;
                    Some((name.to_string(), cents))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Usage {
        currency: v
            .pointer("/meta/currency")
            .and_then(|c| c.as_str())
            .unwrap_or("USD")
            .to_string(),
        spend_cents: summary.get("current_spend_cents")?.as_i64()?,
        bandwidth_pct: i("/data/summary/bandwidth/usage_percentage"),
        credits: i("/data/summary/credits/used_cents").zip(i("/data/summary/credits/total_cents")),
        alert: i("/data/summary/alert/threshold_cents")
            .zip(i("/data/summary/alert/remaining_percentage")),
        applications,
    })
}

pub fn fmt_money(cents: i64, currency: &str) -> String {
    let symbol = match currency.to_ascii_uppercase().as_str() {
        "USD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        _ => "",
    };
    let amount = format!("{:.2}", cents as f64 / 100.0);
    if symbol.is_empty() {
        format!("{amount} {currency}")
    } else {
        format!("{symbol}{amount}")
    }
}

/// `lc-menubar/0.1.0 (macOS 27.2; arm64)`: product, version and platform only.
pub fn user_agent() -> String {
    // Computed once: `sw_vers` is a subprocess.
    static UA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    UA.get_or_init(|| {
        format!(
            "lc-menubar/{} (macOS {}; {})",
            env!("CARGO_PKG_VERSION"),
            macos_version().unwrap_or_else(|| "unknown".into()),
            std::env::consts::ARCH
        )
    })
    .clone()
}

fn macos_version() -> Option<String> {
    let out = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    let v = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    token: String,
}

impl Client {
    pub fn new(token: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent())
            .timeout(std::time::Duration::from_secs(20))
            .build()?;
        Ok(Self { http, token })
    }

    async fn get<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            bail!("Unauthorized: check your API token");
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "GET {url} → {status}: {}",
                crate::model::truncate(&body, 200)
            );
        }
        resp.json().await.with_context(|| format!("decoding {url}"))
    }

    async fn get_all<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<Resource<T>>> {
        let mut url = format!("{BASE}{path}");
        let mut out = vec![];
        for _ in 0..20 {
            let page: Page<T> = self.get(&url).await?;
            out.extend(page.data);
            match page.links.and_then(|l| l.next) {
                // The token goes with every request: only follow links to the API.
                Some(next) if next.starts_with(&format!("{BASE}/")) => url = next,
                Some(next) => bail!("unexpected pagination link: {next}"),
                None => break,
            }
        }
        Ok(out)
    }

    /// Applications and their environments (without deployments).
    pub async fn topology(&self) -> Result<Vec<Application>> {
        let apps = self.get_all::<AppAttrs>("/applications").await?;
        let mut out = vec![];
        for app in apps {
            let envs = self
                .get_all::<EnvAttrs>(&format!("/applications/{}/environments", app.id))
                .await?;
            out.push(Application {
                id: app.id.clone(),
                account: String::new(),
                org: String::new(),
                name: app.attributes.name,
                default_branch: app
                    .attributes
                    .repository
                    .as_ref()
                    .and_then(|r| r.default_branch.clone()),
                environments: envs
                    .into_iter()
                    .map(|e| Environment {
                        id: e.id,
                        name: e.attributes.name,
                        status: e.attributes.status,
                        domain: e.attributes.vanity_domain.filter(|d| !d.is_empty()),
                        latest: None,
                        metrics: None,
                        recent_branches: vec![],
                    })
                    .collect(),
            });
        }
        Ok(out)
    }

    /// The newest deployment, and the branches of the recent ones (newest first).
    pub async fn latest_deployment(
        &self,
        env_id: &str,
    ) -> Result<(Option<Deployment>, Vec<String>)> {
        let page: Page<DeployAttrs> = self
            .get(&format!("{BASE}/environments/{env_id}/deployments"))
            .await?;
        let deps: Vec<Deployment> = page.data.into_iter().map(to_deployment).collect();
        let mut by_time: Vec<&Deployment> = deps.iter().collect();
        by_time.sort_by_key(|d| std::cmp::Reverse(d.started_at.or(d.finished_at)));
        let mut branches: Vec<String> = vec![];
        for d in by_time {
            if !d.branch.is_empty() && !branches.contains(&d.branch) && branches.len() < 8 {
                branches.push(d.branch.clone());
            }
        }
        Ok((newest(deps), branches))
    }

    /// Point the environment at another branch (applies to the next deployment).
    pub async fn set_branch(&self, env_id: &str, branch: &str) -> Result<()> {
        let url = format!("{BASE}/environments/{env_id}");
        let resp = self
            .http
            .patch(&url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .json(&serde_json::json!({ "branch": branch }))
            .send()
            .await
            .with_context(|| format!("PATCH {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("{status}: {}", api_error(&text));
        }
        Ok(())
    }

    pub async fn metrics(&self, env_id: &str, period: Period) -> Result<Metrics> {
        let resp: MetricsResp = self
            .get(&format!(
                "{BASE}/environments/{env_id}/metrics?period={}",
                period.as_str()
            ))
            .await?;
        Ok(parse_metrics(resp, period))
    }

    pub async fn variables(&self, env_id: &str) -> Result<Vec<(String, String)>> {
        let v: serde_json::Value = self.get(&format!("{BASE}/environments/{env_id}")).await?;
        find_variables(&v).context("response did not contain environment_variables")
    }

    async fn post_json(&self, url: &str, body: serde_json::Value) -> Result<()> {
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("{status}: {}", api_error(&body));
        }
        Ok(())
    }

    /// Insert or update variables (matched by key).
    pub async fn set_variables(&self, env_id: &str, vars: &[(String, String)]) -> Result<()> {
        let url = format!("{BASE}/environments/{env_id}/variables");
        for chunk in vars.chunks(200) {
            let variables: Vec<_> = chunk
                .iter()
                .map(|(k, v)| serde_json::json!({ "key": k, "value": v }))
                .collect();
            self.post_json(
                &url,
                serde_json::json!({ "method": "set", "variables": variables }),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn delete_variables(&self, env_id: &str, keys: &[String]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let url = format!("{BASE}/environments/{env_id}/variables/delete");
        self.post_json(&url, serde_json::json!({ "keys": keys }))
            .await
    }

    pub async fn organization(&self) -> Result<String> {
        let v: serde_json::Value = self.get(&format!("{BASE}/meta/organization")).await?;
        v.pointer("/data/attributes/name")
            .and_then(|n| n.as_str())
            .map(str::to_string)
            .context("organization response without a name")
    }

    pub async fn usage(&self) -> Result<Usage> {
        let v: serde_json::Value = self.get(&format!("{BASE}/usage")).await?;
        parse_usage(&v).context("unexpected /usage response")
    }

    /// One page of environment logs between `from` and `to` (RFC 3339).
    pub async fn logs(&self, env_id: &str, q: &LogQuery) -> Result<serde_json::Value> {
        let mut url = reqwest::Url::parse(&format!("{BASE}/environments/{env_id}/logs"))?;
        {
            let mut p = url.query_pairs_mut();
            p.append_pair("from", &q.from).append_pair("to", &q.to);
            if !q.query.trim().is_empty() {
                p.append_pair("query", q.query.trim());
            }
            if let Some(t) = q.kind.as_deref().filter(|t| !t.is_empty()) {
                p.append_pair("type", t);
            }
            if let Some(c) = q.cursor.as_deref().filter(|c| !c.is_empty()) {
                p.append_pair("cursor", c);
            }
        }
        self.get(url.as_str()).await
    }

    pub async fn run_command(&self, env_id: &str, command: &str) -> Result<serde_json::Value> {
        let url = format!("{BASE}/environments/{env_id}/commands");
        self.post_value(&url, serde_json::json!({ "command": command }))
            .await
    }

    pub async fn command(&self, id: &str) -> Result<serde_json::Value> {
        self.get(&format!("{BASE}/commands/{id}")).await
    }

    pub async fn commands(&self, env_id: &str) -> Result<serde_json::Value> {
        self.get(&format!("{BASE}/environments/{env_id}/commands"))
            .await
    }

    pub async fn start_environment(&self, env_id: &str) -> Result<()> {
        let url = format!("{BASE}/environments/{env_id}/start");
        self.post_value(&url, serde_json::json!({}))
            .await
            .map(|_| ())
    }

    pub async fn stop_environment(&self, env_id: &str) -> Result<()> {
        let url = format!("{BASE}/environments/{env_id}/stop");
        self.post_value(&url, serde_json::json!({}))
            .await
            .map(|_| ())
    }

    pub async fn purge_edge_cache(&self, env_id: &str) -> Result<()> {
        let url = format!("{BASE}/environments/{env_id}/purge-edge-cache");
        self.post_value(&url, serde_json::json!({}))
            .await
            .map(|_| ())
    }

    async fn post_value(&self, url: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("{status}: {}", api_error(&text));
        }
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
    }

    /// Trigger a deployment. Returns a human-readable outcome.
    pub async fn deploy(&self, env_id: &str) -> Result<String> {
        let url = format!("{BASE}/environments/{env_id}/deployments");
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::ACCEPTED {
            return Ok("Deployment queued until resources are ready".into());
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("{status}: {}", crate::model::truncate(&body, 200));
        }
        Ok("Deployment started".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_deployment_page_and_picks_newest() {
        let json = r#"{
          "data": [
            {"id":"dep-old","type":"deployments","attributes":{
              "status":"deployment.succeeded","branch_name":"main","commit_hash":"1111111aaaa",
              "commit_message":"old","commit_author":null,"failure_reason":null,
              "started_at":"2026-11-10T10:00:00Z","finished_at":"2026-11-10T10:02:00Z"}},
            {"id":"dep-new","type":"deployments","attributes":{
              "status":"build.running","branch_name":"main","commit_hash":"2222222bbbb",
              "commit_message":"new","commit_author":"max","failure_reason":null,
              "started_at":"2026-11-11T10:00:00Z","finished_at":null}}
          ],
          "links": {"next": null},
          "meta": {"current_page": 1}
        }"#;
        let page: Page<DeployAttrs> = serde_json::from_str(json).unwrap();
        let d = newest(page.data.into_iter().map(to_deployment).collect()).unwrap();
        assert_eq!(d.id, "dep-new");
        assert_eq!(d.short_hash(), "2222222");
    }

    #[test]
    fn pending_deployment_without_start_is_newest() {
        let mk = |id: &str, started: Option<&str>| Deployment {
            id: id.into(),
            status: "pending".into(),
            branch: String::new(),
            commit_hash: String::new(),
            commit_message: String::new(),
            commit_author: None,
            failure_reason: None,
            started_at: started.map(|s| s.parse().unwrap()),
            finished_at: None,
        };
        let d = newest(vec![mk("a", Some("2026-11-11T10:00:00Z")), mk("b", None)]).unwrap();
        assert_eq!(d.id, "b");
        assert!(newest(vec![]).is_none());
    }

    #[test]
    fn rejected_deployment_without_start_does_not_stick() {
        let mk = |id: &str, status: &str, started: Option<&str>| Deployment {
            id: id.into(),
            status: status.into(),
            branch: String::new(),
            commit_hash: String::new(),
            commit_message: String::new(),
            commit_author: None,
            failure_reason: Some("Deployments are temporarily disabled".into()),
            started_at: started.map(|s| s.parse().unwrap()),
            finished_at: None,
        };
        // An attempt rejected before it started, then a successful deploy.
        let d = newest(vec![
            mk("ok", "deployment.succeeded", Some("2026-11-11T12:00:00Z")),
            mk("rejected", "failed", None),
        ])
        .unwrap();
        assert_eq!(d.id, "ok");
        // Without any times, the API's order decides.
        let d = newest(vec![
            mk("first", "failed", None),
            mk("second", "failed", None),
        ])
        .unwrap();
        assert_eq!(d.id, "first");
    }

    #[test]
    fn parses_metrics_with_errors_and_series() {
        let json = r#"{"data":{
            "cpu_usage":{"labels":["web"],"data":[{"x":"10:00","y":[12.5]},{"x":"10:01","y":[null]},{"x":"10:02","y":[20]}],
                         "average":[16.25],"min":0,"max":100},
            "memory_usage":{"error":true},
            "http_response_count":{"labels":["2xx","5xx"],"data":[{"x":"10:00","y":[100,1]}],
                         "total":101,"average":[100,1],"min":0,"max":100},
            "replica_count":{"labels":[],"data":[{"x":"10:00","y":[2]}],"average":[2],"min":0,"max":3}
          },"meta":{"period":"1h","available_periods":["1h"]}}"#;
        let m = parse_metrics(serde_json::from_str(json).unwrap(), Period::H1);
        let cpu = m.cpu.unwrap();
        assert_eq!(cpu.series[0].label, "web");
        assert_eq!(cpu.series[0].points, vec![12.5, 20.0]);
        assert!(m.memory.is_none());
        let req = m.requests.unwrap();
        assert_eq!(req.total, Some(101.0));
        assert_eq!(req.series[1].label, "5xx");
        assert_eq!(m.replicas.unwrap().series[0].points, vec![2.0]);
        assert!(m.workers.is_none());
    }

    #[test]
    fn finds_variables_top_level_or_nested() {
        let flat = serde_json::json!({"data":{"id":"e","attributes":{"name":"p",
            "environment_variables":[{"key":"APP_ENV","value":"production"},{"key":"N","value":null}]}}});
        assert_eq!(
            find_variables(&flat).unwrap(),
            vec![
                ("APP_ENV".into(), "production".into()),
                ("N".into(), String::new())
            ]
        );
        let nested = serde_json::json!({"data":{"attributes":{"name":"p",
            "":{"environment_variables":[{"key":"A","value":"1"}]}}}});
        assert_eq!(
            find_variables(&nested).unwrap(),
            vec![("A".into(), "1".into())]
        );
        let none = serde_json::json!({"data":{"attributes":{"name":"p"}}});
        assert!(find_variables(&none).is_none());
    }

    #[test]
    fn parses_usage() {
        let v = serde_json::json!({"data":{"summary":{"current_spend_cents":1234,
            "bandwidth":{"cost_cents":0,"usage_percentage":42,"allowance_bytes":1},
            "credits":null,"alert":{"threshold_cents":5000,"remaining_percentage":75}},
            "application_totals":{"total_cost_cents":1000,"application_count":1,
              "applications":[{"name":"Shop","total_cost_cents":1000},{"weird":true}]}},
            "meta":{"currency":"EUR","period":0,"available_periods":[],"last_updated_at":"x"}});
        let u = parse_usage(&v).unwrap();
        assert_eq!(u.spend_cents, 1234);
        assert_eq!(u.bandwidth_pct, Some(42));
        assert_eq!(u.credits, None);
        assert_eq!(u.alert, Some((5000, 75)));
        assert_eq!(u.applications, vec![("Shop".to_string(), 1000)]);
        assert_eq!(fmt_money(u.spend_cents, &u.currency), "€12.34");
        assert_eq!(fmt_money(5, "CHF"), "0.05 CHF");
    }

    #[test]
    fn user_agent_names_product_version_platform() {
        let ua = user_agent();
        assert!(
            ua.starts_with(&format!("lc-menubar/{} (macOS ", env!("CARGO_PKG_VERSION"))),
            "{ua}"
        );
        assert!(
            ua.ends_with(&format!("; {})", std::env::consts::ARCH)),
            "{ua}"
        );
    }

    #[test]
    fn api_errors_are_readable() {
        assert_eq!(
            api_error(
                r#"{"message":"The command field is required.","errors":{"command":["The command field is required."]}}"#
            ),
            "The command field is required."
        );
        assert_eq!(
            api_error(r#"{"message":"Invalid.","errors":{"keys":["Missing: FOO"]}}"#),
            "Invalid. Missing: FOO"
        );
        assert_eq!(api_error("oops"), "oops");
    }

    #[test]
    fn parses_environments_with_missing_optional_fields() {
        let json = r#"{"data":[{"id":"env-1","type":"environments",
            "attributes":{"name":"production","status":"running","vanity_domain":"shop.laravel.cloud"}}]}"#;
        let page: Page<EnvAttrs> = serde_json::from_str(json).unwrap();
        assert_eq!(
            page.data[0].attributes.vanity_domain.as_deref(),
            Some("shop.laravel.cloud")
        );
        assert!(page.links.is_none());
    }
}
