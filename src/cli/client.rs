use std::time::Duration;

use reqwest::{Method, RequestBuilder};

const HTTP_CLIENT_TIMEOUT: Duration = Duration::from_secs(120);
static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

pub struct NameshedApiClient {
    base_uri: String,
}

impl NameshedApiClient {
    pub fn new(base_uri: String) -> Self {
        NameshedApiClient { base_uri }
    }

    pub fn base_uri(&self) -> &str {
        &self.base_uri
    }

    pub fn request(&self, method: Method, s: &str) -> RequestBuilder {
        // TODO: make sure that base_uri doesnt end with a slash and s doesnt
        // start with an s?
        let path = if s.starts_with('/') {
            format!("{}{}", self.base_uri, s)
        } else {
            format!("{}/{}", self.base_uri, s)
        };

        let client = reqwest::ClientBuilder::new()
            .user_agent(APP_USER_AGENT)
            .timeout(HTTP_CLIENT_TIMEOUT)
            .build()
            .unwrap();

        client.request(method, path)
    }

    pub fn get(&self, s: &str) -> RequestBuilder {
        self.request(Method::GET, s)
    }

    pub fn post(&self, s: &str) -> RequestBuilder {
        self.request(Method::POST, s)
    }
}
