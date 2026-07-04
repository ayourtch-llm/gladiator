#[derive(Debug, Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub path: String,
}

impl Endpoint {
    pub fn new(base_url: &str, path: &str) -> Self {
        let base_url = base_url.trim_end_matches('/');
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{}", path)
        };
        Self {
            base_url: base_url.to_string(),
            path,
        }
    }

    pub fn url(&self) -> String {
        format!("{}{}", self.base_url, self.path)
    }
}
