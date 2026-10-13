use gitlab::api::projects::merge_requests::{
    CreateMergeRequest, CreateMergeRequestBuilderError, MergeRequests, MergeRequestsBuilderError,
};
use gitlab::api::users::{CurrentUser, CurrentUserBuilderError};
use gitlab::api::{ApiError, Query, projects};
use gitlab::{Gitlab, GitlabError, RestError};
use regex::Regex;
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

    pub fn find_merge_request_by_key(&self, key: &str) -> Result<Vec<MergeRequest>> {
        let mrs = self.list_merge_requests()?;
        Ok(mrs
            .into_iter()
            .filter(|mr| {
                mr.extract_task().as_deref() == Some(key)
                    || mr.title().contains(key)
                    || mr.source_branch().contains(key)
            })
            .collect())
    }

    /// My merge requests: every open one, plus any updated in the last `days` days.
    /// Merge requests of any state and author opened from `branch`, newest first.
    pub fn find_merge_requests_by_branch(&self, branch: &str) -> Result<Vec<MergeRequest>> {
        let endpoint = MergeRequests::builder()
            .project(self.project.clone())
            .source_branch(branch)
            .build()?;

        let mrs: Vec<MergeRequest> = endpoint.query(&self.client)?;
        Ok(mrs)
    }

    pub fn create_merge_request(
        &self,
        title: impl Into<String>,
        description: impl Into<String>,
        source_branch: impl Into<String>,
    ) -> Result<MergeRequest> {
        let endpoint = CreateMergeRequest::builder()
            .project(self.project.clone())
            .title(title.into())
            .description(description.into())
            .source_branch(source_branch.into())
            .target_branch("master")
            .build()?;

        let mr: MergeRequest = endpoint.query(&self.client)?;
        Ok(mr)
    }
}

/// My merge requests in `project`: all opened, plus any updated in the last
/// `days` days. Plain REST with `scope=created_by_me`, both lists at once: the
/// gitlab crate spends two requests on the token check and the current user first.
pub fn my_merge_requests(
    host: &str,
    token: &str,
    project: &str,
    days: i64,
) -> std::result::Result<Vec<MergeRequest>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let base = format!(
        "https://{host}/api/v4/projects/{}/merge_requests?scope=created_by_me&per_page=100",
        urlencoding::encode(project)
    );
    let since = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
    let get_pages = |query: String, max_pages: u32| {
        let mut mrs: Vec<MergeRequest> = Vec::new();
        for page in 1..=max_pages {
            let response = client
                .get(format!("{base}&{query}&page={page}"))
                .header("PRIVATE-TOKEN", token)
                .send()
                .and_then(|r| r.error_for_status())
                .map_err(|e| e.to_string())?;
            let last = response
                .headers()
                .get("x-next-page")
                .is_none_or(|v| v.is_empty());
            mrs.extend(response.json::<Vec<MergeRequest>>().map_err(|e| e.to_string())?);
            if last {
                break;
            }
        }
        Ok::<_, String>(mrs)
    };
    let (opened, recent) = std::thread::scope(|scope| {
        let opened = scope.spawn(|| get_pages("state=opened".to_string(), 10));
        let recent = get_pages(format!("updated_after={}", urlencoding::encode(&since)), 3);
        (opened.join().unwrap(), recent)
    });
    let mut mrs = opened?;
    for mr in recent? {
        if !mrs.iter().any(|m| m.iid == mr.iid) {
            mrs.push(mr);
        }
    }
    Ok(mrs)
}

/// Review state of an open MR: three more requests, so only for open ones.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MrReview {
    /// Head pipeline status: "running", "success", "failed", "pending"…
    pub pipeline: Option<String>,
    pub approved_by: Vec<String>,
    /// Resolvable discussions not resolved yet.
    pub unresolved: usize,
}

pub fn mr_review(host: &str, token: &str, project: &str, iid: u64) -> std::result::Result<MrReview, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let base = format!(
        "https://{host}/api/v4/projects/{}/merge_requests/{iid}",
        urlencoding::encode(project)
    );
    let get = |path: &str| -> std::result::Result<serde_json::Value, String> {
        client
            .get(format!("{base}{path}"))
            .header("PRIVATE-TOKEN", token)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json())
            .map_err(|e| e.to_string())
    };
    let (mr, approvals, discussions) = std::thread::scope(|scope| {
        let approvals = scope.spawn(|| get("/approvals"));
        let discussions = scope.spawn(|| get("/discussions?per_page=100"));
        (get(""), approvals.join().unwrap(), discussions.join().unwrap())
    });
    let pipeline = mr?
        .pointer("/head_pipeline/status")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let approved_by = approvals?
        .get("approved_by")
        .and_then(|v| v.as_array())
        .map(|users| {
            users
                .iter()
                .filter_map(|u| u.pointer("/user/username").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let unresolved = discussions?
        .as_array()
        .map(|threads| {
            threads
                .iter()
                .filter(|d| {
                    d.pointer("/notes/0/resolvable").and_then(|v| v.as_bool()) == Some(true)
                        && d.pointer("/notes/0/resolved").and_then(|v| v.as_bool()) == Some(false)
                })
                .count()
        })
        .unwrap_or(0);
    Ok(MrReview {
        pipeline,
        approved_by,
        unresolved,
    })
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct User {
    id: u64,
    username: String,
    name: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MergeRequest {
    iid: u64,
    title: String,
    state: String,
    author: Option<User>,
    web_url: String,
    source_branch: String,
    sha: Option<String>,
    /// GitLab's verdict on what blocks the merge: "mergeable", "ci_still_running",
    /// "ci_must_pass", "discussions_not_resolved", "not_approved", "conflict"…
    #[serde(default)]
    pub detailed_merge_status: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub created_at: Option<String>,
}

impl MergeRequest {
    pub fn iid(&self) -> u64 {
        self.iid
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn extract_task(&self) -> Option<String> {
        let re = Regex::new(r"\b[A-Z]+-\d+\b").unwrap();
        re.find_iter(&self.title)
            .map(|m| m.as_str().to_string())
            .into_iter()
            .next()
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

    /// Head commit of the source branch when the MR was last updated.
    pub fn sha(&self) -> Option<&str> {
        self.sha.as_deref()
    }
}
