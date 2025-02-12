use {
    crate::driver_model::{execute, solve},
    anyhow::{anyhow, Context, Result},
    reqwest::Client,
    shared::http_client::response_body_with_size_limit,
    std::time::Duration,
    url::Url,
};

const RESPONSE_SIZE_LIMIT: usize = 10_000_000;
const RESPONSE_TIME_LIMIT: Duration = Duration::from_secs(60);

pub struct Driver {
    url: Url,
    client: Client,
}

impl Driver {
    pub fn new(url: Url) -> Self {
        Self {
            url,
            client: Client::builder()
                .timeout(RESPONSE_TIME_LIMIT)
                .build()
                .unwrap(),
        }
    }

    pub async fn solve(&self, request: &solve::Request) -> Result<solve::Response> {
        self.request_response(&["solve"], Some(request)).await
    }

    pub async fn execute(
        &self,
        solution_id: &str,
        _request: &execute::Request,
    ) -> Result<execute::Response> {
        // TODO: should be execute
        self.request_response(&["settle", solution_id], Option::<&()>::None)
            .await
    }

    async fn request_response<Response>(
        &self,
        path: &[&str],
        request: Option<&impl serde::Serialize>,
    ) -> Result<Response>
    where
        Response: serde::de::DeserializeOwned,
    {
        let mut url = self.url.clone();
        let mut segments = url.path_segments_mut().unwrap();
        for path in path {
            segments.push(path);
        }
        std::mem::drop(segments);
        let request = if let Some(request) = request {
            tracing::trace!(
                path=&url.path(),
                body=%serde_json::to_string_pretty(request).unwrap(),
                "request",
            );
            self.client.post(url).json(request)
        } else {
            tracing::trace!(path=%url.path(), "request");
            self.client.post(url)
        };
        let mut response = request.send().await.context("send")?;
        let status = response.status().as_u16();
        let body = response_body_with_size_limit(&mut response, RESPONSE_SIZE_LIMIT)
            .await
            .context("body")?;
        let text = String::from_utf8_lossy(&body);
        tracing::trace!(body=%text, "response");
        if status != 200 {
            let body = std::str::from_utf8(&body).context("body text")?;
            return Err(anyhow!("bad status {}, body {:?}", status, body));
        }
        serde_json::from_slice(&body).with_context(|| format!("body json: {text}"))
    }
}
