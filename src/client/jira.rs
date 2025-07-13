use chrono::{DateTime, FixedOffset};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;

#[derive(Debug)]
pub struct JiraClient {
    client: Client,
    base_url: String,
    token: String,
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

    /// Выполняет произвольный JQL-запрос и возвращает задачи
    pub fn search_issues(&self, jql: &str) -> Result<Vec<JiraIssue>, Box<dyn std::error::Error>> {
        let encoded_jql = urlencoding::encode(jql);
        let url = format!("{}/rest/api/2/search?jql={}", self.base_url, encoded_jql);

        let response = self.client.get(&url).headers(self.auth_headers()).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            return Err(format!("Request failed ({}): {}", status, text).into());
        }

        // let body = response.text()?;                      // consumes the response
        // println!("Debug raw response:\n{}", body);        // print for debug
        // let parsed: JiraSearchResult = serde_json::from_str(&body)?;  // parse from buffer
        // Ok(parsed.issues)

        let search_result: JiraSearchResult = response.json()?;
        Ok(search_result.issues)
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

#[derive(Debug, Deserialize)]
pub struct JiraIssueFields {
    pub summary: String,
    pub description: Option<String>,
    pub created: DateTime<FixedOffset>,
    pub updated: DateTime<FixedOffset>,
    pub status: Option<Status>,
    pub assignee: Option<Assignee>,
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
