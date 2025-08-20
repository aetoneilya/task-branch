use gitlab::api::projects::merge_requests::{
    CreateMergeRequest, CreateMergeRequestBuilderError, MergeRequests, MergeRequestsBuilderError,
};
use gitlab::api::users::{CurrentUser, CurrentUserBuilderError};
use gitlab::api::{ApiError, Query, projects};
use gitlab::{Gitlab, GitlabError, RestError};
use thiserror::Error;

#[derive(Debug)]
pub struct GitLabClient {
    client: Gitlab,
    project: String,
}

#[derive(Debug, Error)]
pub enum GitLabError {
    #[error("failed during execution of gitlab command: {0}")]
    ClientCreationError(#[from] GitlabError),

    #[error("can't build current user query {0}")]
    CurrentUserBuilderError(#[from] CurrentUserBuilderError),

    #[error("can't build merge request query {0}")]
    MergeRequestsBuilderError(#[from] MergeRequestsBuilderError),

    #[error("can't build merge request query {0}")]
    CreateMergeRequestBuilderError(#[from] CreateMergeRequestBuilderError),

    #[error("error during execution of gitlab command: {0}")]
    ApiError(#[from] ApiError<RestError>),
}

pub(crate) type Result<T> = std::result::Result<T, GitLabError>;

impl GitLabClient {
    pub fn create(base_url: String, token: String, project: String) -> Result<Self> {
        let client = Gitlab::new(base_url, token)?;

        let gl_client = GitLabClient { client, project };

        Ok(gl_client)
    }

    pub fn current_user(&self) -> Result<User> {
        let user: User = CurrentUser::builder().build()?.query(&self.client)?;
        Ok(user)
    }

    pub fn list_merge_requests(&self) -> Result<Vec<MergeRequest>> {
        let current_user = self.current_user()?;

        let endpoint = MergeRequests::builder()
            .project(self.project.clone())
            .state(projects::merge_requests::MergeRequestState::Opened)
            .author(current_user.id)
            .build()?;

        let mrs: Vec<MergeRequest> = endpoint.query(&self.client)?;

        Ok(mrs)
    }

    pub fn create_merge_request(
        &self,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<MergeRequest> {
        let endpoint = CreateMergeRequest::builder()
            .project(self.project.clone())
            .title(title.into())
            .description(description.into())
            .build()?;

        let mr = endpoint.query(&self.client)?;

        Ok(mr)
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct User {
    id: u64,
    username: String,
    name: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct MergeRequest {
    iid: u64,
    title: String,
    state: String,
    author: Option<User>,
    web_url: String,
    source_branch: String,
}

impl MergeRequest {
    pub fn iid(&self) -> u64 {
        self.iid
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    pub fn author(&self) -> &Option<User> {
        &self.author
    }

    pub fn web_url(&self) -> &str {
        &self.web_url
    }

    pub fn source_branch(&self) -> &str {
        &self.source_branch
    }
}
