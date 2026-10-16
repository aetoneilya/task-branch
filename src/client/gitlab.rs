use reqwest::Url;
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// GitLab's REST API with one HTTP client: requests share its connections
/// (HTTP/2 multiplexes them over one), so a dozen MRs cost one TLS handshake.
pub struct GitLab {
    client: Client,
    host: String,
}

/// The value of a scoped thread, or an error when it panicked.
fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, Result<T, String>>) -> Result<T, String> {
    handle
        .join()
        .unwrap_or_else(|_| Err("a GitLab request panicked".to_string()))
}

impl GitLab {
    pub fn new(host: &str, token: &str) -> Result<GitLab, String> {
        let mut token = HeaderValue::from_str(token)
            .map_err(|e| format!("GITLAB_TOKEN can't be sent in a header: {e}"))?;
        token.set_sensitive(true);
        let headers = HeaderMap::from_iter([(HeaderName::from_static("private-token"), token)]);
        let client = Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(GitLab {
            client,
            host: host.to_string(),
        })
    }

    /// `https://<host>/api/v4/projects/<project>/<path…>`; the project path is one
    /// segment, its slashes encoded.
    fn project_url(&self, project: &str, path: &[&str]) -> Result<Url, String> {
        let mut url = Url::parse(&format!("https://{}/api/v4/projects", self.host))
            .map_err(|e| e.to_string())?;
        url.path_segments_mut()
            .map_err(|_| format!("bad GitLab host {}", self.host))?
            .push(project)
            .extend(path);
        Ok(url)
    }

    fn get(&self, url: Url, query: &[(&str, &str)]) -> Result<Response, String> {
        self.client
            .get(url)
            .query(query)
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())
    }

    /// My open merge requests in `project`. Plain REST with `scope=created_by_me`:
    /// the gitlab crate spends two requests on the token check and the current
    /// user first.
    pub fn my_open_merge_requests(&self, project: &str) -> Result<Vec<MergeRequest>, String> {
        self.my_merge_requests(project, ("state", "opened"), 10)
    }

    /// My merge requests in `project` updated in the last `days` days, open or not.
    pub fn my_recent_merge_requests(
        &self,
        project: &str,
        days: i64,
    ) -> Result<Vec<MergeRequest>, String> {
        let since = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        self.my_merge_requests(project, ("updated_after", &since), 3)
    }

    fn my_merge_requests(
        &self,
        project: &str,
        filter: (&str, &str),
        max_pages: u32,
    ) -> Result<Vec<MergeRequest>, String> {
        let url = self.project_url(project, &["merge_requests"])?;
        let mut mrs: Vec<MergeRequest> = Vec::new();
        for page in 1..=max_pages {
            let page = page.to_string();
            let query = [
                ("scope", "created_by_me"),
                ("per_page", "100"),
                filter,
                ("page", page.as_str()),
            ];
            let response = self.get(url.clone(), &query)?;
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
        let iid = iid.to_string();
        let get = |tail: &[&str], query: &[(&str, &str)]| -> Result<serde_json::Value, String> {
            let path = [&["merge_requests", iid.as_str()], tail].concat();
            self.get(self.project_url(project, &path)?, query)?
                .json()
                .map_err(|e| e.to_string())
        };
        let (mr, approvals, discussions) = std::thread::scope(|scope| {
            let approvals = scope.spawn(|| get(&["approvals"], &[]));
            let discussions = scope.spawn(|| get(&["discussions"], &[("per_page", "100")]));
            (get(&[], &[]), joined(approvals), joined(discussions))
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
pub struct MergeRequest {
    iid: u64,
    title: String,
    state: MrState,
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

#[cfg(test)]
mod tests {
    use super::GitLab;

    #[test]
    fn project_path_is_one_segment() {
        let gitlab = GitLab::new("gitlab.example", "token").unwrap();
        let url = gitlab
            .project_url("ai/ucp", &["merge_requests", "7", "approvals"])
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://gitlab.example/api/v4/projects/ai%2Fucp/merge_requests/7/approvals"
        );
        let empty: &[(&str, &str)] = &[];
        let request = gitlab.client.get(url.clone()).query(empty).build().unwrap();
        assert_eq!(request.url().query(), None);
        let request = gitlab
            .client
            .get(url)
            .query(&[("updated_after", "2026-10-01T00:00:00+00:00")])
            .build()
            .unwrap();
        assert_eq!(
            request.url().query(),
            Some("updated_after=2026-10-01T00%3A00%3A00%2B00%3A00")
        );
    }
}
