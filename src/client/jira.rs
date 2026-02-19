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

    #[error("git command exited with status {0}")]
    RequestResultError(StatusCode, String),
}

pub(crate) type Result<T> = std::result::Result<T, JiraError>;

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

    pub fn get_issue_by_key(&self, key: &str) -> Result<JiraIssue> {
        let url = format!("{}/rest/api/2/issue/{}", self.base_url, key);
        self.get_json(&url)
    }

    pub fn search_issues(&self, jql: &str) -> Result<Vec<JiraIssue>> {
        let encoded_jql = urlencoding::encode(jql);
        let url = format!("{}/rest/api/2/search?jql={}", self.base_url, encoded_jql);

        let result: JiraSearchResult = self.get_json(&url)?;
        Ok(result.issues)
    }
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
