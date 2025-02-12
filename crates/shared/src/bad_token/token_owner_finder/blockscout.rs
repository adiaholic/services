use {
    super::TokenOwnerProposing,
    anyhow::{bail, Result},
    ethcontract::H160,
    prometheus::IntCounterVec,
    prometheus_metric_storage::MetricStorage,
    reqwest::{Client, Url},
    serde::Deserialize,
};

const BASE: &str = "https://blockscout.com/";

pub struct BlockscoutTokenOwnerFinder {
    client: Client,
    base: Url,
}

impl BlockscoutTokenOwnerFinder {
    pub fn try_with_network(client: Client, network_id: u64) -> Result<Self> {
        let network = match network_id {
            1 => "eth/",
            100 => "xdai/",
            _ => bail!("Unsupported Network"),
        };

        Ok(Self {
            client,
            base: Url::try_from(BASE)
                .expect("Invalid Blockscout Base URL")
                .join(network)
                .expect("Invalid Blockscout URL Segement")
                .join("mainnet/api")
                .expect("Invalid Blockscout URL Segement"),
        })
    }

    async fn query_owners(&self, token: H160) -> Result<Vec<H160>> {
        let mut url = self.base.clone();
        url.query_pairs_mut()
            .append_pair("module", "token")
            .append_pair("action", "getTokenHolders")
            .append_pair("contractaddress", &format!("{token:#x}"));

        tracing::debug!(%url, "Querying Blockscout API");

        let response = self.client.get(url).send().await?;
        let status = response.status();
        let status_result = response.error_for_status_ref().map(|_| ());
        let body = response.text().await?;

        tracing::debug!(%status, %body, "Response from Blockscout API");

        status_result?;
        let parsed = serde_json::from_str::<Response>(&body)?;

        // We technically only need one candidate, returning the top 2 in case there is
        // a race condition and tokens have just been transferred out
        Ok(parsed
            .result
            .into_iter()
            .map(|owner| owner.address)
            .take(2)
            .collect())
    }
}

#[derive(Deserialize)]
struct Response {
    result: Vec<TokenOwner>,
}

#[derive(Deserialize)]
struct TokenOwner {
    address: H160,
}

#[derive(MetricStorage, Clone, Debug)]
#[metric(subsystem = "blockscout_token_owner_finding")]
struct Metrics {
    /// Tracks number of "ok" or "err" responses from blockscout.
    #[metric(labels("result"))]
    results: IntCounterVec,
}

#[async_trait::async_trait]
impl TokenOwnerProposing for BlockscoutTokenOwnerFinder {
    async fn find_candidate_owners(&self, token: H160) -> Result<Vec<H160>> {
        let metric = &Metrics::instance(global_metrics::get_metric_storage_registry())
            .unwrap()
            .results;

        match self.query_owners(token).await {
            Ok(ok) => {
                metric.with_label_values(&["ok"]).inc();
                Ok(ok)
            }
            Err(err) => {
                tracing::warn!(?err, "error finding token owners with Blockscout");
                metric.with_label_values(&["err"]).inc();
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, hex_literal::hex};

    #[tokio::test]
    #[ignore]
    async fn test_blockscout_token_finding_mainnet() {
        let finder = BlockscoutTokenOwnerFinder::try_with_network(Client::default(), 1).unwrap();
        let owners = finder
            .find_candidate_owners(H160(hex!("1337BedC9D22ecbe766dF105c9623922A27963EC")))
            .await;
        assert!(!owners.unwrap().is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn test_blockscout_token_finding_xdai() {
        let finder = BlockscoutTokenOwnerFinder::try_with_network(Client::default(), 100).unwrap();
        let owners = finder
            .find_candidate_owners(H160(hex!("1337BedC9D22ecbe766dF105c9623922A27963EC")))
            .await;
        assert!(!owners.unwrap().is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn test_blockscout_token_finding_no_owners() {
        let finder = BlockscoutTokenOwnerFinder::try_with_network(Client::default(), 100).unwrap();
        let owners = finder
            .find_candidate_owners(H160(hex!("000000000000000000000000000000000000def1")))
            .await;
        assert!(owners.unwrap().is_empty());
    }
}
