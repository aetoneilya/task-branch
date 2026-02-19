use crate::client::git::{GitError, checkout_branch, current_branch};
use crate::client::gitlab::{GitLabClient, GitLabError, MergeRequest, User};
use crate::client::jira;
use crate::client::jira::JiraError::RequestResultError;
use crate::client::jira::{JiraClient, JiraIssue};
use crate::config::{GitLabConfig, JiraConfig};
use crate::core::StateError::{JiraError, MissingToken};
use reqwest::StatusCode;
use std::cell::OnceCell;
use std::path::PathBuf;
use thiserror::Error;

pub struct JiraGitlabState {
    repo_path: PathBuf,
    jira_client: JiraClient,
    gitlab_client: GitLabClient,
    gitlab_user: OnceCell<User>,
}

impl JiraGitlabState {
    pub fn new(
        repo_path: PathBuf,
        jira_config: JiraConfig,
        gitlab_cfg: GitLabConfig,
    ) -> Result<Self> {
        let jira_token =
            std::env::var("JIRA_TOKEN").map_err(|_| MissingToken("JIRA_TOKEN".to_string()))?;
        let gitlab_token =
            std::env::var("GITLAB_TOKEN").map_err(|_| MissingToken("GITLAB_TOKEN".to_string()))?;

        let state = Self {
            repo_path,
            jira_client: JiraClient::new(jira_config.host, jira_token),
            gitlab_client: GitLabClient::create(gitlab_cfg.host, gitlab_token, gitlab_cfg.project)?,
            gitlab_user: OnceCell::new(),
        };

        Ok(state)
    }

    pub fn find_merge_request_by_key(&self, key: &str) -> Result<Vec<MergeRequest>> {
        self.gitlab_client
            .find_merge_request_by_key(key)
            .map_err(StateError::from)
    }

    pub fn list_merge_request(&self) -> Result<Vec<MergeRequest>> {
        self.gitlab_client
            .list_merge_requests()
            .map_err(StateError::from)
    }

    pub fn checkout_to(&self, branch: &str) -> Result<()> {
        checkout_branch(self.repo_path.as_path(), branch).map_err(StateError::from)
    }

    pub fn find_issue_by_key(&self, key: &str) -> Result<Option<JiraIssue>> {
        match self.jira_client.get_issue_by_key(key) {
            Ok(issue) => Ok(Some(issue)),
            Err(RequestResultError(status_code, _)) if status_code == StatusCode::NOT_FOUND => {
                Ok(None)
            }
            Err(e) => Err(JiraError(e)),
        }
    }

    pub fn search_issues(&self, query: &str) -> Result<Vec<JiraIssue>> {
        self.jira_client
            .search_issues(query)
            .map_err(StateError::from)
    }

    pub fn unresolved_current_user_issues(&self) -> Result<Vec<JiraIssue>> {
        let query = "assignee = currentUser() AND resolution = Unresolved";
        self.search_issues(query)
    }

    pub fn create_merge_request(
        &self,
        title: String,
        description: Option<String>,
    ) -> Result<MergeRequest> {
        let branch = current_branch(&self.repo_path)?;
        let description: String = description.unwrap_or_else(String::new);

        self.gitlab_client
            .create_merge_request(title, description, branch)
            .map_err(StateError::from)
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("GitLab error: {0}")]
    GitLabError(#[from] GitLabError),

    #[error("Jira error: {0}")]
    JiraError(#[from] jira::JiraError),

    #[error("Git error: {0}")]
    GitError(#[from] GitError),

    #[error("Environment dont contain toke {0}")]
    MissingToken(String),
}

pub(crate) type Result<T> = std::result::Result<T, StateError>;
