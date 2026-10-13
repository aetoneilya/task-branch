use std::time::Duration;

use crate::client::jira::JiraError::RequestResultError;
use chrono::{DateTime, FixedOffset};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug)]
pub struct JiraClient {
    client: Client,
    base_url: String,
    token: String,
}

#[derive(Debug, Error)]
pub enum JiraError {
    #[error("failed during request execution: {0}")]
    RequestExecutionError(#[from] reqwest::Error),

    #[error("Jira answered {0}")]
    RequestResultError(StatusCode, String),
}

pub(crate) type Result<T> = std::result::Result<T, JiraError>;

impl JiraError {
    /// Jira refused the query itself (400), e.g. a `key in (…)` with an unknown key.
    pub fn bad_request(&self) -> bool {
        matches!(self, JiraError::RequestResultError(status, _) if *status == StatusCode::BAD_REQUEST)
    }
}

impl JiraClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        JiraClient {
            client: Client::new(),
            base_url: base_url.into(),
            token: token.into(),
        }
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let auth_value = format!("Bearer {}", self.token);
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&auth_value).unwrap());
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self
            .client
            .get(url)
            .headers(self.auth_headers())
            .timeout(Duration::from_secs(5))
            .send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            return Err(RequestResultError(
                status,
                format!("Request failed ({}): {}", status, text).into(),
            ));
        }

        Ok(response.json()?)
    }

    /// The workflow transitions available on the issue now.
    pub fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let url = format!("{}/rest/api/2/issue/{}/transitions", self.base_url, key);
        let result: Transitions = self.get_json(&url)?;
        Ok(result.transitions)
    }

    pub fn transition(&self, key: &str, transition_id: &str) -> Result<()> {
        let url = format!("{}/rest/api/2/issue/{}/transitions", self.base_url, key);
        let response = self
            .client
            .post(&url)
            .headers(self.auth_headers())
            .timeout(Duration::from_secs(10))
            .json(&serde_json::json!({ "transition": { "id": transition_id } }))
            .send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            return Err(RequestResultError(status, text));
        }
        Ok(())
    }

    pub fn get_issue_by_key(&self, key: &str) -> Result<JiraIssue> {
        let url = format!("{}/rest/api/2/issue/{}", self.base_url, key);
        self.get_json(&url)
    }

    /// Search returning only the fields `JiraIssue` needs, up to `max` issues.
    pub fn search_issues_brief(&self, jql: &str, max: usize) -> Result<Vec<JiraIssue>> {
        let url = format!(
            "{}/rest/api/2/search?jql={}&maxResults={}&fields=summary,status,created,updated,assignee",
            self.base_url,
            urlencoding::encode(jql),
            max
        );
        let result: JiraSearchResult = self.get_json(&url)?;
        Ok(result.issues)
    }

    pub fn search_issues(&self, jql: &str) -> Result<Vec<JiraIssue>> {
        let encoded_jql = urlencoding::encode(jql);
        let url = format!("{}/rest/api/2/search?jql={}", self.base_url, encoded_jql);

        let result: JiraSearchResult = self.get_json(&url)?;
        Ok(result.issues)
    }
}

#[derive(Debug, Deserialize)]
struct Transitions {
    transitions: Vec<Transition>,
}

#[derive(Debug, Deserialize)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to: TransitionTarget,
}

#[derive(Debug, Deserialize)]
pub struct TransitionTarget {
    pub name: String,
    #[serde(rename = "statusCategory")]
    pub status_category: StatusCategory,
}

#[derive(Debug, Deserialize)]
struct JiraSearchResult {
    issues: Vec<JiraIssue>,
}

#[derive(Debug, Deserialize)]
pub struct JiraIssue {
    pub key: String,
    pub id: String,
    #[serde(rename = "self")]
    pub self_link: String,
    pub fields: JiraIssueFields,
}

impl JiraIssue {
    pub fn status_category(&self) -> &str {
        self.fields
            .status
            .as_ref()
            .and_then(|s| s.status_category.as_ref())
            .map(|c| c.key.as_str())
            .unwrap_or("")
    }

    pub fn status(&self) -> &str {
        self.fields
            .status
            .as_ref()
            .map(|s| s.name.as_str())
            .unwrap_or("unknown")
    }
}

#[derive(Debug, Deserialize)]
pub struct JiraIssueFields {
    pub summary: String,
    pub description: Option<String>,
    pub created: DateTime<FixedOffset>,
    pub updated: DateTime<FixedOffset>,
    pub status: Option<Status>,
    pub assignee: Option<Assignee>,
    pub subtasks: Option<Vec<JiraSubtask>>,
}

#[derive(Debug, Deserialize)]
pub struct JiraSubtask {
    pub key: String,
    pub id: String,
    #[serde(rename = "self")]
    pub self_link: String,
    pub fields: JiraSubtaskFields,
}

impl JiraSubtask {
    pub fn status(&self) -> &str {
        self.fields
            .status
            .as_ref()
            .map(|s| s.name.as_str())
            .unwrap_or("unknown")
    }
}

#[derive(Debug, Deserialize)]
pub struct JiraSubtaskFields {
    pub summary: String,
    pub status: Option<Status>,
}

#[derive(Debug, Deserialize)]
pub struct Status {
    pub description: String,
    pub name: String,
    pub id: String,
    #[serde(rename = "statusCategory")]
    pub status_category: Option<StatusCategory>,
}

/// Jira's coarse status: "new", "indeterminate" (in progress) or "done".
#[derive(Debug, Deserialize)]
pub struct StatusCategory {
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct Assignee {
    pub name: String,
    #[serde(rename = "emailAddress")]
    pub email_address: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub active: bool,
}
