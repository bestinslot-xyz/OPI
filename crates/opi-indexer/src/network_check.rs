//! `run`'s comparison of brc20 with the OPI network, for readiness.
//!
//! Every READY_NETWORK_CHECK_SECS the check compares brc20's cumulative
//! event and trace hashes with the ones the OPI network publishes, at the
//! lower of brc20's height and the network's best verified block. The
//! network publishes a height's hashes some time after the block, so the
//! comparison waits at the network's height while brc20 runs ahead. Each
//! completed comparison is recorded with
//! [`opi_core::metrics::Metrics::set_network_check`], and a disagreement
//! makes `/health/ready` answer 503. A comparison that cannot complete, the
//! network being unreachable say, is logged and records nothing.

use {
    crate::validate::{Ours, Theirs},
    anyhow::{Result, anyhow},
    opi_brc20::{client::EventProviderClient, config::Brc20IndexerConfig, database::Brc20Database},
    opi_core::metrics::NetworkCheck,
    std::time::Duration,
};

/// brc20's database and the OPI network's API (OPI_API_URL), for the
/// network of `config`.
pub struct Brc20Sides {
    db: Brc20Database,
    client: EventProviderClient,
}

impl Brc20Sides {
    /// Opens a pool on brc20's database; call it inside a tokio runtime.
    pub fn new(config: &Brc20IndexerConfig) -> Result<Brc20Sides> {
        Ok(Brc20Sides {
            db: Brc20Database::new(config),
            client: EventProviderClient::new(config).map_err(|err| anyhow!("{err}"))?,
        })
    }
}

#[async_trait::async_trait]
impl Sides for Brc20Sides {
    async fn local_height(&self) -> Result<i32> {
        self.db
            .get_current_block_height()
            .await
            .map_err(|err| anyhow!("reading brc20's latest height: {err}"))
    }

    async fn best_verified(&self) -> Result<i32> {
        self.client
            .get_best_verified_block()
            .await
            .map_err(|err| anyhow!("reading the network's best verified block: {err}"))
    }

    async fn ours(&self, height: i32) -> Result<Option<Ours>> {
        let read = |err| anyhow!("reading brc20's hashes at height {height}: {err}");
        let Some(event) = self
            .db
            .get_cumulative_events_hash(height)
            .await
            .map_err(read)?
        else {
            return Ok(None);
        };
        let trace = self
            .db
            .get_cumulative_traces_hash(height)
            .await
            .map_err(read)?
            .unwrap_or_default();
        Ok(Some(Ours { event, trace }))
    }

    async fn theirs(&self, height: i32) -> Result<Option<Theirs>> {
        let block = self
            .client
            .get_block_info(height)
            .await
            .map_err(|err| anyhow!("reading the network's hashes at height {height}: {err}"))?;
        Ok(Some(Theirs {
            event: block.best_cumulative_hash,
            trace: block.best_cumulative_trace_hash,
        }))
    }
}

/// What the comparison reads: brc20's side and the network's.
#[async_trait::async_trait]
pub trait Sides: Send + Sync {
    /// brc20's latest indexed height.
    async fn local_height(&self) -> Result<i32>;
    /// The network's best verified block, 0 when it has none.
    async fn best_verified(&self) -> Result<i32>;
    /// brc20's cumulative hashes at `height`, `None` when it has none there.
    async fn ours(&self, height: i32) -> Result<Option<Ours>>;
    /// The network's cumulative hashes at `height`, `None` when it has none
    /// there.
    async fn theirs(&self, height: i32) -> Result<Option<Theirs>>;
}

/// One comparison, at the lower of brc20's height and the network's best
/// verified block. `None` when there is nothing to compare: that height is
/// below `first_height` (brc20's first BRC-20 height), or either side has no
/// hashes there.
pub async fn check_once(sides: &dyn Sides, first_height: i32) -> Result<Option<NetworkCheck>> {
    let verified = sides.best_verified().await?;
    let height = sides.local_height().await?.min(verified);
    if height < first_height {
        return Ok(None);
    }
    let Some(ours) = sides.ours(height).await? else {
        return Ok(None);
    };
    let Some(theirs) = sides.theirs(height).await? else {
        return Ok(None);
    };
    Ok(Some(NetworkCheck {
        height: u32::try_from(height)?,
        agrees: ours.matches(&theirs),
    }))
}

/// Compares every `every`, recording each completed comparison in the
/// process metrics, until the task is aborted.
pub async fn run(sides: Box<dyn Sides>, first_height: i32, every: Duration) {
    let mut last: Option<NetworkCheck> = None;
    loop {
        match check_once(sides.as_ref(), first_height).await {
            Ok(Some(check)) => {
                if last.is_none_or(|last| last.agrees != check.agrees) {
                    if check.agrees {
                        tracing::info!(
                            "brc20 agrees with the OPI network at height {}",
                            check.height
                        );
                    } else {
                        tracing::error!(
                            "brc20 disagrees with the OPI network at height {}; readiness answers 503",
                            check.height
                        );
                    }
                }
                opi_core::metrics::global().set_network_check(check);
                last = Some(check);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!("comparing brc20 with the OPI network: {err:#}"),
        }
        tokio::time::sleep(every).await;
    }
}

#[cfg(test)]
mod tests {
    use {super::*, anyhow::anyhow, std::sync::Mutex};

    /// Sides whose hashes are `event-<height>` and `trace-<height>`, the
    /// network's differing from `diverge_at` on.
    struct Fake {
        local: i32,
        verified: Result<i32, String>,
        diverge_at: i32,
        missing: Option<i32>,
        asked: Mutex<Vec<(&'static str, i32)>>,
    }

    impl Fake {
        fn new(local: i32, verified: i32) -> Fake {
            Fake {
                local,
                verified: Ok(verified),
                diverge_at: i32::MAX,
                missing: None,
                asked: Mutex::default(),
            }
        }

        fn asked(&self) -> Vec<(&'static str, i32)> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Sides for Fake {
        async fn local_height(&self) -> Result<i32> {
            Ok(self.local)
        }

        async fn best_verified(&self) -> Result<i32> {
            self.verified.clone().map_err(|err| anyhow!(err))
        }

        async fn ours(&self, height: i32) -> Result<Option<Ours>> {
            self.asked.lock().unwrap().push(("ours", height));
            Ok((self.missing != Some(height)).then(|| Ours {
                event: format!("event-{height}"),
                trace: format!("trace-{height}"),
            }))
        }

        async fn theirs(&self, height: i32) -> Result<Option<Theirs>> {
            self.asked.lock().unwrap().push(("theirs", height));
            let event = if height >= self.diverge_at {
                format!("other-{height}")
            } else {
                format!("event-{height}")
            };
            Ok(Some(Theirs {
                event,
                trace: Some(format!("trace-{height}")),
            }))
        }
    }

    const FIRST: i32 = 10;

    #[tokio::test]
    async fn compares_at_the_networks_height_while_brc20_runs_ahead() {
        let sides = Fake::new(120, 100);
        assert_eq!(
            check_once(&sides, FIRST).await.unwrap(),
            Some(NetworkCheck {
                height: 100,
                agrees: true
            })
        );
        assert_eq!(sides.asked(), [("ours", 100), ("theirs", 100)]);
    }

    #[tokio::test]
    async fn compares_at_brc20s_height_while_the_network_runs_ahead() {
        let sides = Fake::new(90, 100);
        assert_eq!(
            check_once(&sides, FIRST).await.unwrap(),
            Some(NetworkCheck {
                height: 90,
                agrees: true
            })
        );
    }

    #[tokio::test]
    async fn a_different_hash_is_a_disagreement() {
        let sides = Fake {
            diverge_at: 50,
            ..Fake::new(120, 100)
        };
        assert_eq!(
            check_once(&sides, FIRST).await.unwrap(),
            Some(NetworkCheck {
                height: 100,
                agrees: false
            })
        );
    }

    #[tokio::test]
    async fn nothing_to_compare_records_nothing() {
        // the network has no verified block yet
        assert_eq!(check_once(&Fake::new(120, 0), FIRST).await.unwrap(), None);
        // below brc20's first height
        assert_eq!(check_once(&Fake::new(5, 100), FIRST).await.unwrap(), None);
        // brc20 has no hash at the height
        let sides = Fake {
            missing: Some(100),
            ..Fake::new(120, 100)
        };
        assert_eq!(check_once(&sides, FIRST).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_unreachable_network_is_an_error() {
        let sides = Fake {
            verified: Err("connection refused".into()),
            ..Fake::new(120, 100)
        };
        let err = check_once(&sides, FIRST).await.unwrap_err();
        assert!(err.to_string().contains("connection refused"), "{err}");
        assert!(sides.asked().is_empty());
    }
}
