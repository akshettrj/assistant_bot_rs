//! Exchange rates fetched automatically: the European Central Bank's daily
//! rates, through [Frankfurter](https://frankfurter.dev), cached by day.
//!
//! A trip's fixed rates and a rate given for an entry come first (see
//! [`super::service::known_rate`]); these are the fallback.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use chrono::NaiveDate;
use futures::future::BoxFuture;
use rust_decimal::Decimal;
use serde::Deserialize;

use super::money::{Currency, Rate};

const FRANKFURTER: &str = "https://api.frankfurter.dev/v1";
const TIMEOUT: Duration = Duration::from_secs(10);
/// Past this many cached rates, the cache starts over.
const CACHE_LIMIT: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub enum RateError {
    #[error("the rates service failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the rates service answered oddly: {0}")]
    Invalid(String),
}

/// Where exchange rates come from.
pub trait RateSource: Send + Sync {
    /// Units of `to` per unit of `from` on day `on` (or the last day before
    /// with a rate); `None` if the source doesn't know one of the currencies.
    fn rate(
        &self,
        from: Currency,
        to: Currency,
        on: NaiveDate,
    ) -> BoxFuture<'_, Result<Option<Rate>, RateError>>;
}

/// The ECB's reference rates, from Frankfurter: about 30 currencies.
pub struct Frankfurter {
    client: reqwest::Client,
    url: String,
}

impl Frankfurter {
    pub fn new() -> Self {
        Self::at(FRANKFURTER)
    }

    /// Frankfurter at `url`, e.g. a self-hosted one.
    pub fn at(url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("the HTTP client builds");
        Self {
            client,
            url: url.trim_end_matches('/').to_string(),
        }
    }
}

impl Default for Frankfurter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct Response {
    rates: HashMap<String, serde_json::Number>,
}

impl RateSource for Frankfurter {
    fn rate(
        &self,
        from: Currency,
        to: Currency,
        on: NaiveDate,
    ) -> BoxFuture<'_, Result<Option<Rate>, RateError>> {
        Box::pin(async move {
            let response = self
                .client
                .get(format!("{}/{on}", self.url))
                .query(&[("base", from.code()), ("symbols", to.code())])
                .send()
                .await?;
            // Unknown currencies are "not found".
            if matches!(response.status().as_u16(), 404 | 422) {
                return Ok(None);
            }
            let body: Response = response.error_for_status()?.json().await?;
            body.rates
                .get(to.code())
                .map(|number| parse_rate(&number.to_string()))
                .transpose()
        })
    }
}

/// A rate as written in JSON: `83.12`, or `5.3e-3`.
fn parse_rate(text: &str) -> Result<Rate, RateError> {
    let value = Decimal::from_str_exact(text)
        .or_else(|_| Decimal::from_scientific(text))
        .map_err(|_| RateError::Invalid(format!("`{text}` is not a rate")))?;
    Rate::new(value).map_err(|error| RateError::Invalid(error.to_string()))
}

/// Rates from a source, remembered by day: an entry's rate is looked up
/// once, and a failure never blocks logging (a rate is then asked for).
pub struct Rates {
    source: Option<Arc<dyn RateSource>>,
    cache: Mutex<HashMap<(Currency, Currency, NaiveDate), Option<Rate>>>,
}

impl Rates {
    pub fn new(source: Arc<dyn RateSource>) -> Self {
        Self {
            source: Some(source),
            cache: Mutex::default(),
        }
    }

    /// No automatic rates at all.
    pub fn none() -> Self {
        Self {
            source: None,
            cache: Mutex::default(),
        }
    }

    /// The rate from `from` to `to` on day `on`, if the source knows it.
    pub async fn get(&self, from: Currency, to: Currency, on: NaiveDate) -> Option<Rate> {
        let source = self.source.as_ref()?;
        let key = (from, to, on);
        if let Some(known) = self.cache().get(&key) {
            return *known;
        }
        match source.rate(from, to, on).await {
            Ok(rate) => {
                let mut cache = self.cache();
                if cache.len() >= CACHE_LIMIT {
                    cache.clear();
                }
                cache.insert(key, rate);
                rate
            }
            // Not cached: the next entry tries again.
            Err(error) => {
                tracing::warn!(%error, %from, %to, %on, "no automatic exchange rate");
                None
            }
        }
    }

    fn cache(&self) -> MutexGuard<'_, HashMap<(Currency, Currency, NaiveDate), Option<Rate>>> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl std::fmt::Debug for Rates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rates")
            .field("automatic", &self.source.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A source with fixed rates, counting the lookups; `fail` makes every
    /// lookup fail.
    #[derive(Default)]
    pub struct FixedRates {
        pub rates: HashMap<(&'static str, &'static str), Decimal>,
        pub fail: bool,
        pub lookups: AtomicUsize,
    }

    impl RateSource for FixedRates {
        fn rate(
            &self,
            from: Currency,
            to: Currency,
            _on: NaiveDate,
        ) -> BoxFuture<'_, Result<Option<Rate>, RateError>> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            let found = self
                .rates
                .iter()
                .find(|((a, b), _)| *a == from.code() && *b == to.code())
                .map(|(_, rate)| Rate::new(*rate).unwrap());
            let fail = self.fail;
            Box::pin(async move {
                if fail {
                    Err(RateError::Invalid("down".into()))
                } else {
                    Ok(found)
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use rust_decimal::dec;

    use super::{testing::FixedRates, *};

    fn currency(code: &str) -> Currency {
        Currency::from_code(code).unwrap()
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()
    }

    #[test]
    fn rates_parse_from_json_numbers() {
        assert_eq!(parse_rate("83.1234").unwrap().value(), dec!(83.1234));
        assert_eq!(parse_rate("5.3e-3").unwrap().value(), dec!(0.0053));
        assert!(parse_rate("0").is_err());
        assert!(parse_rate("abc").is_err());
    }

    #[tokio::test]
    async fn rates_are_cached_by_day_but_failures_are_not() {
        let source = Arc::new(FixedRates {
            rates: [(("USD", "INR"), dec!(83.5))].into(),
            ..FixedRates::default()
        });
        let rates = Rates::new(Arc::clone(&source) as Arc<dyn RateSource>);
        let usd_inr = rates.get(currency("USD"), currency("INR"), day()).await;
        assert_eq!(usd_inr.map(Rate::value), Some(dec!(83.5)));
        rates.get(currency("USD"), currency("INR"), day()).await;
        // Unknown pairs are remembered too.
        assert_eq!(
            rates.get(currency("VND"), currency("INR"), day()).await,
            None
        );
        rates.get(currency("VND"), currency("INR"), day()).await;
        assert_eq!(source.lookups.load(Ordering::SeqCst), 2);

        let failing = Arc::new(FixedRates {
            fail: true,
            ..FixedRates::default()
        });
        let rates = Rates::new(Arc::clone(&failing) as Arc<dyn RateSource>);
        assert_eq!(
            rates.get(currency("USD"), currency("INR"), day()).await,
            None
        );
        rates.get(currency("USD"), currency("INR"), day()).await;
        assert_eq!(failing.lookups.load(Ordering::SeqCst), 2);

        assert_eq!(
            Rates::none()
                .get(currency("USD"), currency("INR"), day())
                .await,
            None
        );
    }

    /// Asks the real Frankfurter: `cargo test -- --ignored frankfurter`.
    #[tokio::test]
    #[ignore = "needs the network"]
    async fn frankfurter_knows_the_dollar() {
        let rate = Frankfurter::new()
            .rate(currency("USD"), currency("INR"), day())
            .await
            .unwrap()
            .unwrap();
        assert!(
            rate.value() > dec!(50) && rate.value() < dec!(150),
            "{rate}"
        );
        let unknown = Frankfurter::new()
            .rate(currency("VND"), currency("INR"), day())
            .await
            .unwrap();
        assert_eq!(unknown, None);
    }
}
