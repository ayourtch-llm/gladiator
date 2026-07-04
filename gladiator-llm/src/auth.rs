use reqwest::RequestBuilder;

pub trait Auth: Send + Sync {
    fn apply(&self, builder: RequestBuilder) -> RequestBuilder;
}

pub struct BearerAuth {
    pub token: String,
}

impl Auth for BearerAuth {
    fn apply(&self, builder: RequestBuilder) -> RequestBuilder {
        builder.header("Authorization", format!("Bearer {}", self.token))
    }
}

pub struct NoneAuth;

impl Auth for NoneAuth {
    fn apply(&self, builder: RequestBuilder) -> RequestBuilder {
        builder
    }
}
