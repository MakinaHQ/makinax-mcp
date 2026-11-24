//! Swap quoting: the adapter trait, best-quote selection, and the built-in
//! keyless aggregator adapters (`fetch-kyber`, `fetch-odos`).
//!
//! CRITICAL addressing invariant (verified against MakinaXModule source,
//! `_swapForSafe`/`_swap`): the swap executes FROM THE MODULE's context and
//! output MUST land at the MODULE (balance-delta measurement + fee skim +
//! forwarding to the Safe). Aggregator sender AND receiver are therefore the
//! MODULE address — never the Safe. Output sent straight to the Safe measures
//! as zero at the module and reverts AmountOutTooLow.

use std::sync::Arc;

use alloy::primitives::{Address, Bytes, U256};
use serde::Serialize;

use crate::config::FetchConfig;
use crate::error::{Error, Result};

/// The client name the built-in adapters send by default (HTTP
/// `User-Agent`, and Kyber's `source` field). A consumer passes its own via
/// each adapter's `new`.
pub(crate) const DEFAULT_CLIENT_NAME: &str = "makinax";

#[derive(Debug, Clone, Serialize)]
pub struct Quote {
    pub adapter: &'static str,
    pub input_token: Address,
    pub output_token: Address,
    pub input_amount: U256,
    pub expected_out: U256,
    /// expected_out scaled down by the requested slippage; the on-chain
    /// minOutputAmount backstop.
    pub min_out: U256,
    /// Router calldata with sender/receiver = MODULE (see module docs).
    pub data: Bytes,
    pub execution_target: Address,
    pub approval_target: Address,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_impact_pct: Option<f64>,
    /// Adapter-suggested freshness window.
    pub valid_hint_s: u64,
    /// The slippage the quote was requested with — retained so an expired
    /// quote can be re-quoted with identical parameters.
    #[serde(default)]
    pub slippage_bps: u16,
}

pub struct QuoteRequest<'a> {
    pub chain: &'a str,
    pub input_token: Address,
    pub output_token: Address,
    pub input_amount: U256,
    pub slippage_bps: u64,
    /// The MODULE address — sender AND receiver of the swap.
    pub module: Address,
}

/// A swap price source. The built-in adapters implement it; a consumer
/// implements it to add or replace one (pass its own list to
/// [`best_quote`] / [`quote_all`]).
#[async_trait::async_trait]
pub trait QuoteAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    async fn quote(&self, req: &QuoteRequest<'_>) -> Result<Quote>;
}

#[derive(Debug, Clone, Serialize)]
pub struct QuoteSummary {
    pub adapter: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_out: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub chosen: bool,
}

pub fn min_out_for(expected: U256, slippage_bps: u64) -> U256 {
    expected * U256::from(10_000u64.saturating_sub(slippage_bps)) / U256::from(10_000u64)
}

/// What an adapter's failure IS, so a caller can say it in those terms.
///
/// A 2 USDC probe on HyperEVM once came back as "odos: unsupported chain
/// hyperevm; kyber: quoted amount is smaller than estimated" — one adapter
/// that does not serve the chain and one that will not route the amount —
/// and the reader concluded swaps were not authorised there. Neither failure
/// is about permission; both are about the price source. Classified from the
/// adapters' own wording, which is the only evidence there is, and
/// `source_error` when the wording is not one recognised here.
pub fn classify_quote_error(error: &str) -> &'static str {
    let e = error.to_ascii_lowercase();
    if e.contains("unsupported chain") || e.contains("not supported on") {
        "not_on_this_chain"
    } else if e.contains("smaller than")
        || e.contains("too small")
        || e.contains("below minimum")
        || e.contains("amount too low")
    {
        "amount_too_small"
    } else if e.contains("timeout") || e.contains("timed out") {
        "timed_out"
    } else {
        "source_error"
    }
}

/// Query all adapters in parallel: the best quote if any adapter produced
/// one, and every adapter's summary either way. A total failure is NOT an
/// error here — it is an answer about the price sources, and the caller
/// decides what to say about it (`best_quote` below turns it into an error
/// for a caller that cannot proceed without a price).
pub async fn quote_all(
    adapters: &[Arc<dyn QuoteAdapter>],
    req: &QuoteRequest<'_>,
) -> (Option<Quote>, Vec<QuoteSummary>) {
    let futures: Vec<_> = adapters
        .iter()
        .map(|a| {
            let a = a.clone();
            async move {
                (
                    a.name(),
                    tokio::time::timeout(std::time::Duration::from_secs(10), a.quote(req)).await,
                )
            }
        })
        .collect();
    let results = futures::future::join_all(futures).await;

    let mut quotes: Vec<Quote> = Vec::new();
    let mut summaries: Vec<QuoteSummary> = Vec::new();
    for (name, res) in results {
        match res {
            Ok(Ok(q)) => {
                summaries.push(QuoteSummary {
                    adapter: name,
                    expected_out: Some(q.expected_out.to_string()),
                    error: None,
                    chosen: false,
                });
                quotes.push(q);
            }
            Ok(Err(e)) => summaries.push(QuoteSummary {
                adapter: name,
                expected_out: None,
                error: Some(crate::util::redact::text(&e.to_string())),
                chosen: false,
            }),
            Err(_) => summaries.push(QuoteSummary {
                adapter: name,
                expected_out: None,
                error: Some("timeout after 10s".into()),
                chosen: false,
            }),
        }
    }

    let best = quotes
        .into_iter()
        .max_by(|a, b| a.expected_out.cmp(&b.expected_out));
    if let Some(best) = &best {
        for s in &mut summaries {
            if s.adapter == best.adapter {
                s.chosen = true;
            }
        }
    }
    (best, summaries)
}

/// Query all adapters in parallel; best = max expected_out among successes.
/// One adapter failing must never sink the other; all failing is an error
/// naming each adapter's problem. For a caller that cannot go on without a
/// price (a simulation or execution); a caller that reports prices uses
/// `quote_all` instead, because for a reader a total failure is a fact to
/// report in structure, not an error to throw.
pub async fn best_quote(
    adapters: &[Arc<dyn QuoteAdapter>],
    req: &QuoteRequest<'_>,
) -> Result<(Quote, Vec<QuoteSummary>)> {
    let (best, summaries) = quote_all(adapters, req).await;
    let best = best.ok_or_else(|| {
        let errs: Vec<String> = summaries
            .iter()
            .map(|s| format!("{}: {}", s.adapter, s.error.clone().unwrap_or_default()))
            .collect();
        Error::Rpc(format!("all quote adapters failed — {}", errs.join("; ")))
    })?;
    Ok((best, summaries))
}

/// The built-in adapters this build carries, minus any the config switches
/// off (`None` = on). Order is stable (odos, then kyber): it is the order
/// `quote_all` reports summaries in.
pub fn default_adapters(cfg: &FetchConfig) -> Vec<Arc<dyn QuoteAdapter>> {
    default_adapters_named(cfg, DEFAULT_CLIENT_NAME)
}

/// [`default_adapters`], with the client name each adapter sends.
#[allow(unused_variables)]
pub fn default_adapters_named(cfg: &FetchConfig, client_name: &str) -> Vec<Arc<dyn QuoteAdapter>> {
    #[allow(unused_mut)]
    let mut out: Vec<Arc<dyn QuoteAdapter>> = Vec::new();
    #[cfg(feature = "fetch-odos")]
    if cfg.odos != Some(false) {
        out.push(Arc::new(super::odos::Odos::new(client_name)));
    }
    #[cfg(feature = "fetch-kyber")]
    if cfg.kyber != Some(false) {
        out.push(Arc::new(super::kyber::Kyber::new(client_name)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(adapter: &'static str, out: u64) -> Quote {
        Quote {
            slippage_bps: 50,
            adapter,
            input_token: Address::repeat_byte(1),
            output_token: Address::repeat_byte(2),
            input_amount: U256::from(100u64),
            expected_out: U256::from(out),
            min_out: min_out_for(U256::from(out), 50),
            data: Bytes::from(vec![1, 2]),
            execution_target: Address::repeat_byte(3),
            approval_target: Address::repeat_byte(3),
            price_impact_pct: None,
            valid_hint_s: 30,
        }
    }

    struct Fixed(&'static str, Option<u64>);
    #[async_trait::async_trait]
    impl QuoteAdapter for Fixed {
        fn name(&self) -> &'static str {
            self.0
        }
        async fn quote(&self, _req: &QuoteRequest<'_>) -> Result<Quote> {
            match self.1 {
                Some(out) => Ok(q(self.0, out)),
                None => Err(Error::Rpc(format!("{} deliberately failing", self.0))),
            }
        }
    }

    fn req() -> QuoteRequest<'static> {
        QuoteRequest {
            chain: "mainnet",
            input_token: Address::repeat_byte(1),
            output_token: Address::repeat_byte(2),
            input_amount: U256::from(100u64),
            slippage_bps: 50,
            module: Address::repeat_byte(9),
        }
    }

    #[test]
    fn min_out_math() {
        assert_eq!(min_out_for(U256::from(10_000u64), 50), U256::from(9_950u64));
        assert_eq!(min_out_for(U256::from(10_000u64), 0), U256::from(10_000u64));
        assert_eq!(min_out_for(U256::from(3u64), 50), U256::from(2u64)); // floor
    }

    #[tokio::test]
    async fn best_selection_prefers_max_output() {
        let adapters: Vec<Arc<dyn QuoteAdapter>> = vec![
            Arc::new(Fixed("odos", Some(100))),
            Arc::new(Fixed("kyber", Some(120))),
        ];
        let (best, summaries) = best_quote(&adapters, &req()).await.unwrap();
        assert_eq!(best.adapter, "kyber");
        assert!(
            summaries
                .iter()
                .find(|s| s.adapter == "kyber")
                .unwrap()
                .chosen
        );
        assert!(
            !summaries
                .iter()
                .find(|s| s.adapter == "odos")
                .unwrap()
                .chosen
        );
    }

    #[tokio::test]
    async fn one_failure_does_not_sink_the_other() {
        let adapters: Vec<Arc<dyn QuoteAdapter>> = vec![
            Arc::new(Fixed("odos", None)),
            Arc::new(Fixed("kyber", Some(80))),
        ];
        let (best, summaries) = best_quote(&adapters, &req()).await.unwrap();
        assert_eq!(best.adapter, "kyber");
        let odos = summaries.iter().find(|s| s.adapter == "odos").unwrap();
        assert!(
            odos.error
                .as_deref()
                .unwrap()
                .contains("deliberately failing")
        );
    }

    /// Each built-in adapter is present when compiled in and not switched off
    /// (`None` = on), in the stable odos-then-kyber order.
    #[test]
    fn default_adapters_honour_features_and_switches() {
        let names = |cfg: &FetchConfig| -> Vec<&'static str> {
            default_adapters(cfg).iter().map(|a| a.name()).collect()
        };
        let mut all = Vec::new();
        if cfg!(feature = "fetch-odos") {
            all.push("odos");
        }
        if cfg!(feature = "fetch-kyber") {
            all.push("kyber");
        }
        assert_eq!(names(&FetchConfig::default()), all);
        let no_kyber = FetchConfig {
            kyber: Some(false),
            odos: Some(true),
            ..Default::default()
        };
        assert!(!names(&no_kyber).contains(&"kyber"));
        let none = FetchConfig {
            kyber: Some(false),
            odos: Some(false),
            ..Default::default()
        };
        assert!(names(&none).is_empty());
    }

    /// The two strings a real probe produced, classified as what they are —
    /// a coverage gap and an amount floor — and not as anything about
    /// permission. Wording not recognised here stays
    /// `source_error` rather than being guessed into a category.
    #[test]
    fn adapter_failures_are_classified_from_their_own_wording() {
        assert_eq!(
            classify_quote_error("unsupported chain `hyperevm`"),
            "not_on_this_chain"
        );
        assert_eq!(
            classify_quote_error("quoted amount is smaller than estimated"),
            "amount_too_small"
        );
        assert_eq!(classify_quote_error("timeout after 10s"), "timed_out");
        assert_eq!(
            classify_quote_error("HTTP 503 from the router"),
            "source_error",
            "unrecognised wording is not guessed into a category"
        );
    }

    /// A total failure is an ANSWER from `quote_all`: no quote, and every
    /// adapter's reason, so a caller can report the price sources instead of
    /// throwing. NON-VACUITY: with one adapter succeeding the same call
    /// returns that quote, so the `None` is the failure and not the shape.
    #[tokio::test]
    async fn quote_all_answers_a_total_failure_with_every_reason() {
        let failing: Vec<Arc<dyn QuoteAdapter>> = vec![
            Arc::new(Fixed("odos", None)),
            Arc::new(Fixed("kyber", None)),
        ];
        let (best, summaries) = quote_all(&failing, &req()).await;
        assert!(best.is_none(), "nothing quoted");
        assert_eq!(summaries.len(), 2, "every adapter is accounted for");
        assert!(
            summaries.iter().all(|s| s.error.is_some() && !s.chosen),
            "{summaries:?}"
        );

        let one_works: Vec<Arc<dyn QuoteAdapter>> = vec![
            Arc::new(Fixed("odos", None)),
            Arc::new(Fixed("kyber", Some(80))),
        ];
        let (best, summaries) = quote_all(&one_works, &req()).await;
        assert_eq!(best.map(|q| q.adapter), Some("kyber"));
        assert!(
            summaries
                .iter()
                .find(|s| s.adapter == "kyber")
                .unwrap()
                .chosen,
            "{summaries:?}"
        );
    }

    #[tokio::test]
    async fn all_failing_reports_every_adapter() {
        let adapters: Vec<Arc<dyn QuoteAdapter>> = vec![
            Arc::new(Fixed("odos", None)),
            Arc::new(Fixed("kyber", None)),
        ];
        let err = best_quote(&adapters, &req()).await.unwrap_err().to_string();
        assert!(err.contains("odos") && err.contains("kyber"), "{}", err);
    }

    /// An adapter's failure is reported in its summary. An adapter a caller
    /// brings may quote its own keyed endpoint; the summary masks the key.
    #[tokio::test]
    async fn a_failing_adapters_summary_masks_an_endpoint_key() {
        struct Keyed;
        #[async_trait::async_trait]
        impl QuoteAdapter for Keyed {
            fn name(&self) -> &'static str {
                "keyed"
            }
            async fn quote(&self, _req: &QuoteRequest<'_>) -> Result<Quote> {
                Err(Error::Rpc(
                    "error sending request for url \
                     (https://quotes.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr)"
                        .into(),
                ))
            }
        }
        let adapters: Vec<Arc<dyn QuoteAdapter>> = vec![Arc::new(Keyed)];
        let (best, summaries) = quote_all(&adapters, &req()).await;
        assert!(best.is_none());
        let shown = summaries[0].error.clone().unwrap();
        assert!(!shown.contains("9fKd2Lm4Xp7Qw1Nz8Bt3Yr"), "{shown}");
        assert!(
            shown.contains("quotes.example.com/v2/<redacted>"),
            "{shown}"
        );
        let e = best_quote(&adapters, &req()).await.unwrap_err().to_string();
        assert!(!e.contains("9fKd2Lm4Xp7Qw1Nz8Bt3Yr"), "{e}");
    }
}
