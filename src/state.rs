use crate::client::git::{checkout_branch, GitError};
use crate::client::gitlab::{GitLabClient, GitLabError, MergeRequest, User};
use crate::client::jira::{JiraClient, JiraError, JiraIssue};
use crate::config;
use std::cell::OnceCell;
use std::sync::LazyLock;
use thiserror::Error;

pub struct State {
    jira_client: LazyLock<JiraClient>,
    gitlab_client: LazyLock<GitLabClient>,
    gitlab_user: OnceCell<User>,
}

impl State {
    pub fn new() -> Self {
        Self {
            jira_client: LazyLock::new(|| Self::build_jira_client()),
            gitlab_client: LazyLock::new(|| Self::build_gitlab_client()),
            gitlab_user: OnceCell::new(),
        }
    }

    pub fn list_merge_request(&self) -> Result<Vec<MergeRequest>> {
        self.gitlab_client
            .list_merge_requests()
            .map_err(StateError::from)
    }

    pub fn checkout_to(&self, branch: &str) -> Result<()> {
        checkout_branch(branch).map_err(StateError::from)
    }

    pub fn search_issues(&self, query: &str) -> Result<Vec<JiraIssue>> {
        self.jira_client
            .search_issues(query)
            .map_err(StateError::from)
    }

    pub fn create_merge_request(
        &self,
        title: String,
        description: Option<String>,
    ) -> Result<MergeRequest> {
        self.gitlab_client
            .create_merge_request(title, description.unwrap_or_default())
            .map_err(StateError::from)
    }

    fn build_jira_client() -> JiraClient {
        let cfg = config::JiraConfig::from_env();
        JiraClient::new(cfg.jira_url, cfg.jira_token)
    }

    fn build_gitlab_client() -> GitLabClient {
        let cfg = config::GitLabConfig::from_env();
        GitLabClient::create(cfg.gitlab_url, cfg.gitlab_token, cfg.gitlab_project)
            .expect("Error creating GitLab client")
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("GitLab error: {0}")]
    GitLabError(#[from] GitLabError),

    #[error("Jira error: {0}")]
    JiraError(#[from] JiraError),

    #[error("Git error: {0}")]
    GitError(#[from] GitError),
}

pub(crate) type Result<T> = std::result::Result<T, StateError>;
