/// GitLab's REST API with one HTTP client: requests share its connections
/// (HTTP/2 multiplexes them over one), so a dozen MRs cost one TLS handshake.
pub struct GitLab {
    client: reqwest::blocking::Client,
    host: String,
    token: String,
}

/// The value of a scoped thread, or an error when it panicked.
fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, Result<T, String>>) -> Result<T, String> {
    handle
        .join()
        .unwrap_or_else(|_| Err("a GitLab request panicked".to_string()))
}

impl GitLab {
    pub fn new(host: &str, token: &str) -> Result<GitLab, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(GitLab {
            client,
            host: host.to_string(),
            token: token.to_string(),
        })
    }

    fn get(&self, url: &str) -> Result<reqwest::blocking::Response, String> {
        self.client
            .get(url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())
    }

    /// My open merge requests in `project`. Plain REST with `scope=created_by_me`:
    /// the gitlab crate spends two requests on the token check and the current
    /// user first.
    pub fn my_open_merge_requests(&self, project: &str) -> Result<Vec<MergeRequest>, String> {
        self.my_merge_requests(project, "state=opened", 10)
    }

    /// My merge requests in `project` updated in the last `days` days, open or not.
    pub fn my_recent_merge_requests(
        &self,
        project: &str,
        days: i64,
    ) -> Result<Vec<MergeRequest>, String> {
        let since = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        let query = format!("updated_after={}", urlencoding::encode(&since));
        self.my_merge_requests(project, &query, 3)
    }

    fn my_merge_requests(
        &self,
        project: &str,
        query: &str,
        max_pages: u32,
    ) -> Result<Vec<MergeRequest>, String> {
        let base = format!(
            "https://{}/api/v4/projects/{}/merge_requests?scope=created_by_me&per_page=100&{query}",
            self.host,
            urlencoding::encode(project)
        );
        let mut mrs: Vec<MergeRequest> = Vec::new();
        for page in 1..=max_pages {
            let response = self.get(&format!("{base}&page={page}"))?;
            let last = response
                .headers()
                .get("x-next-page")
                .is_none_or(|v| v.is_empty());
            mrs.extend(
                response
                    .json::<Vec<MergeRequest>>()
                    .map_err(|e| e.to_string())?,
            );
            if last {
                break;
            }
        }
        Ok(mrs)
    }

    pub fn mr_review(&self, project: &str, iid: u64) -> Result<MrReview, String> {
        let base = format!(
            "https://{}/api/v4/projects/{}/merge_requests/{iid}",
            self.host,
            urlencoding::encode(project)
        );
        let get = |path: &str| -> Result<serde_json::Value, String> {
            self.get(&format!("{base}{path}"))?
                .json()
                .map_err(|e| e.to_string())
        };
        let (mr, approvals, discussions) = std::thread::scope(|scope| {
            let approvals = scope.spawn(|| get("/approvals"));
            let discussions = scope.spawn(|| get("/discussions?per_page=100"));
            (get(""), joined(approvals), joined(discussions))
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
                            && d.pointer("/notes/0/resolved").and_then(|v| v.as_bool())
                                == Some(false)
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

/// GitLab's merge request states; anything else is `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MrState {
    Opened,
    Merged,
    Closed,
    Locked,
    #[serde(other)]
    Other,
}

impl MrState {
    pub fn as_str(self) -> &'static str {
        match self {
            MrState::Opened => "opened",
            MrState::Merged => "merged",
            MrState::Closed => "closed",
            MrState::Locked => "locked",
            MrState::Other => "other",
        }
    }
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
    state: MrState,
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

    pub fn state(&self) -> MrState {
        self.state
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
