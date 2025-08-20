use dotenv::dotenv;
use std::env;

pub struct JiraConfig {
    pub jira_token: String,
    pub jira_url: String,
}

pub struct GitLabConfig {
    pub gitlab_token: String,
    pub gitlab_url: String,
    pub gitlab_project: String,
}

impl JiraConfig {
    pub fn from_env() -> Self {
        Self {
            jira_token: env::var("JIRA_TOKEN").expect("JIRA_TOKEN not set"),
            jira_url: env::var("JIRA_BASE_URL").expect("JIRA_BASE_URL not set"),
        }
    }
}

impl GitLabConfig {
    pub fn from_env() -> Self {
        Self {
            gitlab_token: env::var("GITLAB_TOKEN").expect("GITLAB_TOKEN not set"),
            gitlab_url: env::var("GITLAB_BASE_URL").expect("GITLAB_BASE_URL not set"),
            gitlab_project: env::var("GITLAB_PROJECT").expect("GITLAB_PROJECT not set"),
        }
    }
}
