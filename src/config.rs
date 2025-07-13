use std::env;
use dotenv::dotenv;

pub struct Config {
    pub token: String,
    pub base_url: String,
}

impl Config {
    pub fn from_env() -> Self {
        dotenv().ok();
        Self {
            token: env::var("JIRA_TOKEN").expect("JIRA_TOKEN not set"),
            base_url: env::var("JIRA_BASE_URL").expect("JIRA_BASE_URL not set"),
        }
    }
}