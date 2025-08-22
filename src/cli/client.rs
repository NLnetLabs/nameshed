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

    pub fn uri_with(&self, s: &str) -> String {
        // TODO: make sure that base_uri doesnt end with a slash and s doesnt
        // start with an s?
        if s.starts_with('/') {
            format!("{}{}", self.base_uri, s)
        } else {
            format!("{}/{}", self.base_uri, s)
        }
    }
}
