//! Universalis v2 REST client.

use serde::Deserialize;

use crate::cache::DataCenter;

pub const MAX_BATCH: usize = 100;
pub const USER_AGENT: &str = concat!("mbwatch/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error)]
pub enum UniversalisError {
    #[error("not found (HTTP 404): {0}")]
    NotFound(String),
    #[error("rate limited by Universalis (HTTP 429){}", .retry_after.map(|s| format!(", retry after {s}s")).unwrap_or_default())]
    RateLimited { retry_after: Option<u64> },
    #[error("unexpected HTTP {status} from {url}")]
    Status { status: u16, url: String },
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct World {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricePoint {
    pub price: f64,
    pub world_id: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct QuantityPoint {
    pub quantity: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Scoped<T> {
    pub world: Option<T>,
    pub dc: Option<T>,
}

impl<T> Default for Scoped<T> {
    fn default() -> Self {
        Scoped { world: None, dc: None }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QualityStats {
    pub min_listing: Scoped<PricePoint>,
    pub median_listing: Scoped<PricePoint>,
    pub average_sale_price: Scoped<PricePoint>,
    pub daily_sale_velocity: Scoped<QuantityPoint>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemAggregate {
    pub item_id: u32,
    pub nq: Option<QualityStats>,
    pub hq: Option<QualityStats>,
}

impl ItemAggregate {
    pub fn quality(&self, hq: bool) -> Option<&QualityStats> {
        if hq { self.hq.as_ref() } else { self.nq.as_ref() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregatedBatch {
    #[serde(default)]
    pub results: Vec<ItemAggregate>,
    #[serde(default)]
    pub failed_items: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub price_per_unit: u64,
    pub quantity: u32,
    #[serde(default)]
    pub world_name: Option<String>,
    #[serde(default)]
    pub hq: bool,
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
}

impl Client {
    /// `base_url` is e.g. `https://universalis.app`; injectable so tests can use a mock server.
    pub fn new(base_url: &str) -> Result<Self, UniversalisError> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Client { http, base_url: base_url.trim_end_matches('/').to_string() })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<T, UniversalisError> {
        let resp = self.http.get(url).query(query).send().await?;
        let status = resp.status();
        match status.as_u16() {
            404 => Err(UniversalisError::NotFound(url.to_string())),
            429 => {
                let retry_after = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse().ok());
                Err(UniversalisError::RateLimited { retry_after })
            }
            _ if !status.is_success() => {
                Err(UniversalisError::Status { status: status.as_u16(), url: url.to_string() })
            }
            _ => Ok(resp.json().await?),
        }
    }

    pub async fn worlds(&self) -> Result<Vec<World>, UniversalisError> {
        self.get_json(&format!("{}/api/v2/worlds", self.base_url), &[]).await
    }

    pub async fn data_centers(&self) -> Result<Vec<DataCenter>, UniversalisError> {
        self.get_json(&format!("{}/api/v2/data-centers", self.base_url), &[]).await
    }

    pub async fn marketable(&self) -> Result<Vec<u32>, UniversalisError> {
        self.get_json(&format!("{}/api/v2/marketable", self.base_url), &[]).await
    }

    /// Stage 1: aggregated stats, automatically chunked into batches of <= 100 IDs.
    pub async fn aggregated(
        &self,
        scope: &str,
        ids: &[u32],
    ) -> Result<AggregatedBatch, UniversalisError> {
        let scope = encode_segment(scope);
        let mut merged = AggregatedBatch::default();
        for chunk in ids.chunks(MAX_BATCH) {
            let joined = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            let url = format!("{}/api/v2/aggregated/{scope}/{joined}", self.base_url);
            let batch: AggregatedBatch = self.get_json(&url, &[]).await?;
            merged.results.extend(batch.results);
            merged.failed_items.extend(batch.failed_items);
        }
        Ok(merged)
    }

    /// Stage 2: the cheapest current listings for one item.
    pub async fn listings(
        &self,
        scope: &str,
        item_id: u32,
        hq: bool,
    ) -> Result<Vec<Listing>, UniversalisError> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            listings: Vec<Listing>,
        }
        let url = format!("{}/api/v2/{}/{item_id}", self.base_url, encode_segment(scope));
        let resp: Resp = self
            .get_json(
                &url,
                &[
                    ("listings", "5"),
                    ("hq", if hq { "true" } else { "false" }),
                    (
                        "fields",
                        "listings.pricePerUnit,listings.quantity,listings.worldName,listings.hq",
                    ),
                ],
            )
            .await?;
        Ok(resp.listings)
    }
}

fn encode_segment(s: &str) -> String {
    s.trim().replace(' ', "%20")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn aggregated_fixture() -> serde_json::Value {
        json!({
          "results": [
            {
              "itemId": 36115,
              "nq": {
                "minListing": {
                  "world": {"price": 2500, "worldId": 33},
                  "dc": {"price": 1500, "worldId": 21},
                  "region": {"price": 1400, "worldId": 80}
                },
                "averageSalePrice": {"world": {"price": 2400.5}, "dc": {"price": 1600.0}},
                "dailySaleVelocity": {"world": {"quantity": 3.5}, "dc": {"quantity": 12.25}}
              },
              "hq": {
                "minListing": {
                  "world": {"price": 3000, "worldId": 33},
                  "dc": {"price": 1750, "worldId": 21}
                },
                "medianListing": {"world": {"price": 3100}, "dc": {"price": 2000}},
                "averageSalePrice": {"world": {"price": 3050.0}, "dc": {"price": 1900.0}},
                "dailySaleVelocity": {"world": {"quantity": 1.0}, "dc": {"quantity": 4.0}}
              }
            },
            { "itemId": 36116, "nq": {} }
          ],
          "failedItems": [999]
        })
    }

    #[tokio::test]
    async fn sends_custom_user_agent_on_every_request() {
        let server = MockServer::start().await;
        Mock::given(header("user-agent", USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(4)
            .mount(&server)
            .await;
        let c = Client::new(&server.uri()).unwrap();
        c.worlds().await.unwrap();
        c.data_centers().await.unwrap();
        c.marketable().await.unwrap();
        // Body shape doesn't match for listings; only the request header matters here.
        let _ = c.listings("Light", 1, true).await;
        assert!(USER_AGENT.starts_with("mbwatch/"));
    }

    #[tokio::test]
    async fn parses_aggregated_fixture_with_failed_and_missing_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/aggregated/Phoenix/36115,36116,999"))
            .respond_with(ResponseTemplate::new(200).set_body_json(aggregated_fixture()))
            .expect(1)
            .mount(&server)
            .await;
        let c = Client::new(&server.uri()).unwrap();
        let out = c.aggregated("Phoenix", &[36115, 36116, 999]).await.unwrap();

        assert_eq!(out.failed_items, vec![999]);
        assert_eq!(out.results.len(), 2);
        let hq = out.results[0].quality(true).unwrap();
        assert_eq!(hq.min_listing.world.unwrap().price, 3000.0);
        assert_eq!(hq.min_listing.dc.unwrap().world_id, Some(21));
        assert_eq!(hq.median_listing.dc.unwrap().price, 2000.0);
        assert_eq!(hq.average_sale_price.world.unwrap().price, 3050.0);
        assert_eq!(hq.daily_sale_velocity.dc.unwrap().quantity, 4.0);
        let nq = out.results[0].quality(false).unwrap();
        assert!(nq.median_listing.world.is_none()); // missing field tolerated
        // item with empty nq and no hq at all
        assert!(out.results[1].quality(true).is_none());
        assert!(out.results[1].quality(false).unwrap().min_listing.world.is_none());
    }

    #[tokio::test]
    async fn chunks_over_100_ids_and_merges_results() {
        let server = MockServer::start().await;
        let ids: Vec<u32> = (1..=250).collect();
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v2/aggregated/Phoenix/[0-9,]+$"))
            .respond_with(|req: &wiremock::Request| {
                let last = req.url.path().rsplit('/').next().unwrap().to_string();
                let ids: Vec<u32> = last.split(',').map(|s| s.parse().unwrap()).collect();
                assert!(ids.len() <= MAX_BATCH, "batch too big: {}", ids.len());
                let results: Vec<_> = ids.iter().map(|i| json!({"itemId": i, "nq": {}})).collect();
                ResponseTemplate::new(200).set_body_json(json!({"results": results, "failedItems": [ids[0] + 1000]}))
            })
            .expect(3)
            .mount(&server)
            .await;
        let c = Client::new(&server.uri()).unwrap();
        let out = c.aggregated("Phoenix", &ids).await.unwrap();
        assert_eq!(out.results.len(), 250);
        assert_eq!(out.failed_items, vec![1001, 1101, 1201]);
    }

    #[tokio::test]
    async fn empty_id_list_makes_no_requests() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&server).await;
        let out = Client::new(&server.uri()).unwrap().aggregated("Phoenix", &[]).await.unwrap();
        assert!(out.results.is_empty());
    }

    #[tokio::test]
    async fn listings_uses_stage2_query_and_parses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/Light/36115"))
            .and(query_param("listings", "5"))
            .and(query_param("hq", "true"))
            .and(query_param(
                "fields",
                "listings.pricePerUnit,listings.quantity,listings.worldName,listings.hq",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"listings": [
                {"pricePerUnit": 1750, "quantity": 12, "worldName": "Ravana", "hq": true},
                {"pricePerUnit": 1800, "quantity": 3, "worldName": "Phoenix", "hq": true}
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        let l = Client::new(&server.uri()).unwrap().listings("Light", 36115, true).await.unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0], Listing { price_per_unit: 1750, quantity: 12, world_name: Some("Ravana".into()), hq: true });
    }

    #[tokio::test]
    async fn http_404_maps_to_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(404)).mount(&server).await;
        let e = Client::new(&server.uri()).unwrap().listings("Light", 1, false).await.unwrap_err();
        assert!(matches!(e, UniversalisError::NotFound(_)), "{e:?}");
    }

    #[tokio::test]
    async fn http_429_maps_to_rate_limited_with_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
            .mount(&server)
            .await;
        let e = Client::new(&server.uri()).unwrap().aggregated("Phoenix", &[1]).await.unwrap_err();
        assert!(matches!(e, UniversalisError::RateLimited { retry_after: Some(7) }), "{e:?}");
    }

    #[tokio::test]
    async fn other_status_maps_to_status_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(503)).mount(&server).await;
        let e = Client::new(&server.uri()).unwrap().marketable().await.unwrap_err();
        assert!(matches!(e, UniversalisError::Status { status: 503, .. }), "{e:?}");
    }

    #[tokio::test]
    async fn metadata_endpoints_parse() {
        let server = MockServer::start().await;
        Mock::given(path("/api/v2/worlds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": 33, "name": "Phoenix"}])))
            .mount(&server).await;
        Mock::given(path("/api/v2/data-centers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!([{"name": "Light", "region": "Europe", "worlds": [33, 21]}])))
            .mount(&server).await;
        Mock::given(path("/api/v2/marketable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([2, 3, 5])))
            .mount(&server).await;
        let c = Client::new(&format!("{}/", server.uri())).unwrap(); // trailing slash tolerated
        assert_eq!(c.worlds().await.unwrap(), vec![World { id: 33, name: "Phoenix".into() }]);
        assert_eq!(c.data_centers().await.unwrap()[0].worlds, vec![33, 21]);
        assert_eq!(c.marketable().await.unwrap(), vec![2, 3, 5]);
    }
}
