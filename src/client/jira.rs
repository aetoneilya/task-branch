use std::time::Duration;

use chrono::{DateTime, FixedOffset};
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct JiraClient {
    client: Client,
    base_url: String,
}

#[derive(Debug, Error)]
pub enum JiraError {
    #[error("failed during request execution: {0}")]
    RequestExecutionError(#[from] reqwest::Error),

    /// The status and the start of the response body.
    #[error("Jira answered {0}: {1}")]
    RequestResultError(StatusCode, String),

    #[error("JIRA_TOKEN can't be sent in a header: {0}")]
    BadToken(#[from] reqwest::header::InvalidHeaderValue),
}

pub(crate) type Result<T> = std::result::Result<T, JiraError>;

impl JiraError {
    /// Jira refused the query itself (400), e.g. a `key in (…)` with an unknown key.
    pub fn bad_request(&self) -> bool {
        matches!(self, JiraError::RequestResultError(status, _) if *status == StatusCode::BAD_REQUEST)
    }

    /// No such issue, or not one I may see (404).
    pub fn not_found(&self) -> bool {
        matches!(self, JiraError::RequestResultError(status, _) if *status == StatusCode::NOT_FOUND)
    }
}

/// How much of an error response goes into the message: Jira answers with whole
/// HTML pages.
const ERROR_BODY_CHARS: usize = 300;

/// The response when it is a success, otherwise its status and body as an error.
fn checked(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let text = response.text().unwrap_or_default();
    let mut body: String = text.trim().chars().take(ERROR_BODY_CHARS).collect();
    if body.len() < text.trim().len() {
        body.push('…');
    }
    Err(JiraError::RequestResultError(status, body))
}

impl JiraClient {
    /// The token is trimmed: one read from a file or a shell often ends in a newline.
    pub fn new(base_url: impl Into<String>, token: &str) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", token.trim()))?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(5))
            .build()?;
        Ok(JiraClient {
            client,
            base_url: base_url.into(),
        })
    }

    /// A client with the token from `JIRA_TOKEN`.
    pub fn from_env(base_url: &str) -> std::result::Result<Self, String> {
        let token = std::env::var("JIRA_TOKEN").map_err(|_| "JIRA_TOKEN is not set".to_string())?;
        JiraClient::new(base_url, &token).map_err(|err| err.to_string())
    }

    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let response = self.client.get(url).query(query).send()?;
        Ok(checked(response)?.json()?)
    }

    /// The workflow transitions available on the issue now.
    pub fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let url = format!("{}/rest/api/2/issue/{}/transitions", self.base_url, key);
        let result: Transitions = self.get_json(&url, &[])?;
        Ok(result.transitions)
    }

    pub fn transition(&self, key: &str, transition_id: &str) -> Result<()> {
        let url = format!("{}/rest/api/2/issue/{}/transitions", self.base_url, key);
        let response = self
            .client
            .post(&url)
            .timeout(Duration::from_secs(10))
            .json(&serde_json::json!({ "transition": { "id": transition_id } }))
            .send()?;
        checked(response).map(|_| ())
    }

    pub fn get_issue_by_key(&self, key: &str) -> Result<JiraIssue> {
        let url = format!("{}/rest/api/2/issue/{}", self.base_url, key);
        self.get_json(&url, &[])
    }

    /// Search returning only the fields `JiraIssue` needs, up to `max` issues.
    pub fn search_issues_brief(&self, jql: &str, max: usize) -> Result<Vec<JiraIssue>> {
        let url = format!("{}/rest/api/2/search", self.base_url);
        let max = max.to_string();
        let query = [
            ("jql", jql),
            ("maxResults", max.as_str()),
            ("fields", "summary,status,created,updated,assignee"),
        ];
        let result: JiraSearchResult = self.get_json(&url, &query)?;
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
    pub fields: JiraIssueFields,
}

impl JiraIssue {
    pub fn status_category(&self) -> Category {
        self.fields
            .status
            .as_ref()
            .and_then(|s| s.status_category.as_ref())
            .map_or(Category::Unknown, |c| c.key)
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
    pub status: Option<Status>,
}

#[derive(Debug, Deserialize)]
pub struct Status {
    pub name: String,
    #[serde(rename = "statusCategory")]
    pub status_category: Option<StatusCategory>,
}

#[derive(Debug, Deserialize)]
pub struct StatusCategory {
    pub key: Category,
}

/// Jira's coarse status, the `statusCategory` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
pub enum Category {
    #[serde(rename = "new")]
    New,
    #[serde(rename = "indeterminate")]
    InProgress,
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "undefined", other)]
    Unknown,
}
