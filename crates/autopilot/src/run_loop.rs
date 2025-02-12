use {
    crate::{
        database::Postgres,
        driver_api::Driver,
        driver_model::{
            execute,
            solve::{self, Class},
        },
        solvable_orders::SolvableOrdersCache,
    },
    anyhow::{anyhow, Context, Result},
    chrono::Utc,
    model::{
        auction::{Auction, AuctionId},
        order::{LimitOrderClass, OrderClass},
    },
    primitive_types::H256,
    rand::seq::SliceRandom,
    shared::{
        current_block::CurrentBlockStream,
        ethrpc::Web3,
        event_handling::MAX_REORG_BLOCK_COUNT,
    },
    std::{collections::HashSet, sync::Arc, time::Duration},
    tracing::Instrument,
    web3::types::Transaction,
};

const SOLVE_TIME_LIMIT: Duration = Duration::from_secs(15);

pub struct RunLoop {
    pub solvable_orders_cache: Arc<SolvableOrdersCache>,
    pub database: Postgres,
    pub drivers: Vec<Driver>,
    pub current_block: CurrentBlockStream,
    pub web3: Web3,
    pub network_block_interval: Duration,
}

impl RunLoop {
    pub async fn run_forever(&self) -> ! {
        loop {
            self.single_run().await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn single_run(&self) {
        let auction = match self.solvable_orders_cache.current_auction() {
            Some(auction) => auction,
            None => {
                tracing::debug!("no current auction");
                return;
            }
        };
        let id = match self.database.replace_current_auction(&auction).await {
            Ok(id) => id,
            Err(err) => {
                tracing::error!(?err, "failed to replace current auction");
                return;
            }
        };
        self.single_run_(id, &auction)
            .instrument(tracing::info_span!("auction", id))
            .await;
    }

    async fn single_run_(&self, id: AuctionId, auction: &Auction) {
        tracing::info!("solving");
        let mut solutions = self.solve(auction, id).await;

        // Shuffle so that sorting randomly splits ties.
        solutions.shuffle(&mut rand::thread_rng());
        solutions.sort_unstable_by(|left, right| left.1.score.total_cmp(&right.1.score));

        // TODO: Keep going with other solutions until some deadline.
        if let Some((index, solution)) = solutions.pop() {
            tracing::info!("executing with solver {}", index);
            match self
                .execute(auction, id, &self.drivers[index], &solution)
                .await
            {
                Ok(()) => (),
                Err(err) => {
                    tracing::error!(?err, "solver {index} failed to execute");
                }
            }
        }

        // TODO:
        // - Think about what per auction information needs to be permanently
        //   stored. We might want
        // to store the competition information and the full promised solution
        // of the winner.
    }

    /// Returns the successful /solve responses and the index of the solver.
    async fn solve(&self, auction: &Auction, id: AuctionId) -> Vec<(usize, solve::Response)> {
        if auction
            .orders
            .iter()
            .all(|order| match order.metadata.class {
                OrderClass::Market => false,
                OrderClass::Liquidity => true,
                OrderClass::Limit(_) => false,
            })
        {
            return Default::default();
        }

        let request = &solve::Request {
            id,
            orders: auction
                .orders
                .iter()
                .map(|order| {
                    let (class, surplus_fee) = match order.metadata.class {
                        OrderClass::Market => (Class::Market, None),
                        OrderClass::Liquidity => (Class::Liquidity, None),
                        OrderClass::Limit(LimitOrderClass { surplus_fee, .. }) => {
                            (Class::Limit, surplus_fee)
                        }
                    };
                    solve::Order {
                        uid: order.metadata.uid,
                        sell_token: order.data.sell_token,
                        buy_token: order.data.buy_token,
                        sell_amount: order.data.sell_amount,
                        buy_amount: order.data.buy_amount,
                        solver_fee: order.metadata.full_fee_amount,
                        user_fee: order.data.fee_amount,
                        valid_to: order.data.valid_to,
                        kind: order.data.kind,
                        receiver: order.data.receiver,
                        owner: order.metadata.owner,
                        partially_fillable: order.data.partially_fillable,
                        executed: Default::default(),
                        pre_interactions: Default::default(),
                        sell_token_balance: order.data.sell_token_balance,
                        buy_token_balance: order.data.buy_token_balance,
                        class,
                        surplus_fee,
                        app_data: order.data.app_data,
                        reward: Default::default(),
                        signature: order.signature.clone(),
                    }
                })
                .collect(),
            prices: auction.prices.clone(),
            deadline: Utc::now() + chrono::Duration::from_std(SOLVE_TIME_LIMIT).unwrap(),
        };
        let futures = self
            .drivers
            .iter()
            .enumerate()
            .map(|(index, driver)| async move {
                let result =
                    match tokio::time::timeout(SOLVE_TIME_LIMIT, driver.solve(request)).await {
                        Ok(inner) => inner,
                        Err(_) => Err(anyhow!("timeout")),
                    };
                (index, result)
            })
            .collect::<Vec<_>>();
        let results = futures::future::join_all(futures).await;
        results
            .into_iter()
            .filter_map(|(index, result)| match result {
                Ok(result) => Some((index, result)),
                Err(err) => {
                    tracing::warn!(?err, "driver solve error");
                    None
                }
            })
            .collect()
    }

    /// Execute the solver's solution. Returns Ok when the corresponding
    /// transaction has been mined.
    async fn execute(
        &self,
        _auction: &Auction,
        id: AuctionId,
        driver: &Driver,
        solution: &solve::Response,
    ) -> Result<()> {
        let request = execute::Request {
            auction_id: id,
            transaction_identifier: id.to_be_bytes().into(),
        };
        let _response = driver
            .execute(&solution.id, &request)
            .await
            .context("execute")?;
        // TODO: React to deadline expiring.
        let transaction = self
            .wait_for_settlement_transaction(&request.transaction_identifier)
            .await
            .context("wait for settlement transaction")?;
        if let Some(tx) = transaction {
            tracing::debug!("settled in tx {:?}", tx.hash);
        }
        Ok(())
    }

    /// Tries to find a `settle` contract call with calldata ending in `tag`.
    ///
    /// Returns None if no transaction was found within the deadline.
    pub async fn wait_for_settlement_transaction(&self, tag: &[u8]) -> Result<Option<Transaction>> {
        const MAX_WAIT_TIME: Duration = Duration::from_secs(60);
        // Start earlier than current block because there might be a delay when
        // receiving the Solver's /execute response during which it already
        // started broadcasting the tx.
        let start_offset = MAX_REORG_BLOCK_COUNT;
        let max_wait_time_blocks =
            (MAX_WAIT_TIME.as_secs_f32() / self.network_block_interval.as_secs_f32()).ceil() as u64;
        let current = self.current_block.borrow().number;
        let start = current.saturating_sub(start_offset);
        let deadline = current.saturating_add(max_wait_time_blocks);
        tracing::debug!(%current, %start, %deadline, ?tag, "waiting for tag");

        // Use the existing event indexing infrastructure to find the transaction. We
        // query all settlement events in the block range to get tx hashes and
        // query the node for the full calldata.
        //
        // If the block range was large, we would make the query more efficient by
        // moving the starting block up while taking reorgs into account. With
        // the current range of 30 blocks this isn't necessary.
        //
        // We do keep track of hashes we have already seen to reduce load from the node.

        let mut seen_transactions: HashSet<H256> = Default::default();
        loop {
            // This could be a while loop. It isn't, because some care must be taken to not
            // accidentally keep the borrow alive, which would block senders. Technically
            // this is fine with while conditions but this is clearer.
            if self.current_block.borrow().number <= deadline {
                break;
            }
            let mut hashes = self
                .database
                .recent_settlement_tx_hashes(start..deadline + 1)
                .await?;
            hashes.retain(|hash| !seen_transactions.contains(hash));
            for hash in hashes {
                let tx: Option<Transaction> = self
                    .web3
                    .eth()
                    .transaction(web3::types::TransactionId::Hash(hash))
                    .await
                    .with_context(|| format!("web3 transaction {hash:?}"))?;
                let tx: Transaction = match tx {
                    Some(tx) => tx,
                    None => continue,
                };
                if tx.input.0.ends_with(tag) {
                    return Ok(Some(tx));
                }
                seen_transactions.insert(hash);
            }
            // It would be more correct to wait until just after the last event update run,
            // but that is hard to synchronize.
            tokio::time::sleep(self.network_block_interval.div_f32(2.)).await;
        }
        Ok(None)
    }
}
