//! Contains command line arguments and related helpers that are shared between
//! the binaries.

use {
    crate::{
        current_block,
        ethrpc,
        fee_subsidy::cow_token::SubsidyTiers,
        gas_price_estimation::GasEstimatorType,
        price_estimation::PriceEstimatorType,
        rate_limiter::RateLimitingStrategy,
        sources::{
            balancer_v2::BalancerFactoryKind,
            uniswap_v2::UniV2BaselineSourceParameters,
            BaselineSource,
        },
        tenderly_api,
    },
    anyhow::{anyhow, ensure, Context, Result},
    ethcontract::{H160, H256, U256},
    model::app_id::AppId,
    std::{
        collections::HashMap,
        fmt::{self, Display, Formatter},
        num::{NonZeroU64, ParseFloatError},
        str::FromStr,
        time::Duration,
    },
    tracing::level_filters::LevelFilter,
    url::Url,
};

#[macro_export]
macro_rules! logging_args_with_default_filter {
    ($struct_name:ident ,$default_filter:literal) => {
        #[derive(clap::Parser)]
        pub struct $struct_name {
            #[clap(long, env, default_value = $default_filter)]
            pub log_filter: String,

            #[clap(long, env, default_value = "error")]
            pub log_stderr_threshold: LevelFilter,
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Driver {
    pub name: String,
    pub url: Url,
}

// The following arguments are used to configure the order creation process
// The arguments are shared between the orderbook crate and the autopilot crate,
// as both crates can create orders
#[derive(clap::Parser)]
pub struct OrderQuotingArguments {
    #[clap(
        long,
        env,
        default_value = "Baseline",
        value_enum,
        use_value_delimiter = true
    )]
    pub price_estimators: Vec<PriceEstimatorType>,

    /// A list of external drivers used for price estimation in the following
    /// format: `<NAME>|<URL>,<NAME>|<URL>`
    #[clap(long, env, use_value_delimiter = true)]
    pub price_estimation_drivers: Vec<Driver>,

    /// The configured addresses whose orders should be considered liquidity and
    /// not regular user orders.
    ///
    /// These orders have special semantics such as not being considered in the
    /// settlements objective funtion, not receiving any surplus, and being
    /// allowed to place partially fillable orders.
    #[clap(long, env, use_value_delimiter = true)]
    pub liquidity_order_owners: Vec<H160>,

    /// The time period an EIP1271-quote request is valid.
    #[clap(
        long,
        env,
        default_value = "600",
        value_parser = duration_from_seconds,
    )]
    pub eip1271_onchain_quote_validity_seconds: Duration,

    /// The time period an PRESIGN-quote request is valid.
    #[clap(
        long,
        env,
        default_value = "600",
        value_parser = duration_from_seconds,
    )]
    pub presign_onchain_quote_validity_seconds: Duration,

    /// A flat fee discount denominated in the network's native token (i.e.
    /// Ether for Mainnet).
    ///
    /// Note that flat fee discounts are applied BEFORE any multiplicative
    /// factors from either `--fee-factor` or
    /// `--partner-additional-fee-factors` configuration.
    #[clap(long, env, default_value = "0")]
    pub fee_discount: f64,

    /// The minimum value for the discounted fee in the network's native token
    /// (i.e. Ether for Mainnet).
    ///
    /// Note that this minimum is applied BEFORE any multiplicative factors from
    /// either `--fee-factor` or `--partner-additional-fee-factors`
    /// configuration.
    #[clap(long, env, default_value = "0")]
    pub min_discounted_fee: f64,

    /// Gas Fee Factor: 1.0 means cost is forwarded to users alteration, 0.9
    /// means there is a 10% subsidy, 1.1 means users pay 10% in fees than
    /// what we estimate we pay for gas.
    #[clap(long, env, default_value = "1", value_parser = parse_unbounded_factor)]
    pub fee_factor: f64,

    /// Used to specify additional fee subsidy factor based on app_ids contained
    /// in orders. Should take the form of a json string as shown in the
    /// following example:
    ///
    /// '0x0000000000000000000000000000000000000000000000000000000000000000:0.5,
    /// $PROJECT_APP_ID:0.7'
    ///
    /// Furthermore, a value of
    /// - 1 means no subsidy and is the default for all app_data not contained
    ///   in this list.
    /// - 0.5 means that this project pays only 50% of the estimated fees.
    #[clap(
        long,
        env,
        default_value = "",
        value_parser = parse_partner_fee_factor,
    )]
    pub partner_additional_fee_factors: HashMap<AppId, f64>,

    /// Used to configure how much of the regular fee a user should pay based on
    /// their COW + VCOW balance in base units on the current network.
    ///
    /// The expected format is "10:0.75,150:0.5" for 2 subsidy tiers.
    /// A balance of [10,150) COW will cause you to pay 75% of the regular fee
    /// and a balance of [150, inf) COW will cause you to pay 50% of the
    /// regular fee.
    #[clap(long, env)]
    pub cow_fee_factors: Option<SubsidyTiers>,
}

logging_args_with_default_filter!(
    LoggingArguments,
    "warn,autopilot=debug,driver=debug,orderbook=debug,solver=debug,shared=debug"
);

#[derive(clap::Parser)]
#[group(skip)]
pub struct Arguments {
    #[clap(flatten)]
    pub ethrpc: ethrpc::Arguments,

    #[clap(flatten)]
    pub current_block: current_block::Arguments,

    #[clap(flatten)]
    pub tenderly: tenderly_api::Arguments,

    #[clap(flatten)]
    pub logging: LoggingArguments,

    /// The Ethereum node URL to connect to.
    #[clap(long, env, default_value = "http://localhost:8545")]
    pub node_url: Url,

    /// The expected chain ID that the services are expected to run against.
    /// This can be optionally specified in order to check at startup whether
    /// the connected nodes match to detect misconfigurations.
    #[clap(long, env)]
    pub chain_id: Option<u64>,

    /// Which gas estimators to use. Multiple estimators are used in sequence if
    /// a previous one fails. Individual estimators support different
    /// networks. `EthGasStation`: supports mainnet.
    /// `GasNow`: supports mainnet.
    /// `GnosisSafe`: supports mainnet and goerli.
    /// `Web3`: supports every network.
    /// `Native`: supports every network.
    #[clap(
        long,
        env,
        default_value = "Web3",
        value_enum,
        ignore_case = true,
        use_value_delimiter = true
    )]
    pub gas_estimators: Vec<GasEstimatorType>,

    /// BlockNative requires api key to work. Optional since BlockNative could
    /// be skipped in gas estimators.
    #[clap(long, env)]
    pub blocknative_api_key: Option<String>,

    /// Base tokens used for finding multi-hop paths between multiple AMMs
    /// Should be the most liquid tokens of the given network.
    #[clap(long, env, use_value_delimiter = true)]
    pub base_tokens: Vec<H160>,

    /// Which Liquidity sources to be used by Price Estimator.
    #[clap(long, env, value_enum, ignore_case = true, use_value_delimiter = true)]
    pub baseline_sources: Option<Vec<BaselineSource>>,

    /// List of non hardcoded univ2-like contracts.
    #[clap(long, env, value_enum, ignore_case = true, use_value_delimiter = true)]
    pub custom_univ2_baseline_sources: Vec<UniV2BaselineSourceParameters>,

    /// The number of blocks kept in the pool cache.
    #[clap(long, env, default_value = "10")]
    pub pool_cache_blocks: NonZeroU64,

    /// The number of pairs that are automatically updated in the pool cache.
    #[clap(long, env, default_value = "4")]
    pub pool_cache_maximum_recent_block_age: u64,

    /// How often to retry requests in the pool cache.
    #[clap(long, env, default_value = "5")]
    pub pool_cache_maximum_retries: u32,

    /// How long to sleep in seconds between retries in the pool cache.
    #[clap(long, env, default_value = "1", value_parser = duration_from_seconds)]
    pub pool_cache_delay_between_retries_seconds: Duration,

    /// Special partner authentication for Paraswap API (allowing higher rater
    /// limits)
    #[clap(long, env)]
    pub paraswap_partner: Option<String>,

    /// The list of disabled ParaSwap DEXs. By default, the `ParaSwapPool4`
    /// DEX (representing a private market maker) is disabled as it increases
    /// price by 1% if built transactions don't actually get executed.
    #[clap(long, env, default_value = "ParaSwapPool4", use_value_delimiter = true)]
    pub disabled_paraswap_dexs: Vec<String>,

    /// Configures the back off strategy for the paraswap API when our requests
    /// get rate limited. Requests issued while back off is active get
    /// dropped entirely. Needs to be passed as
    /// "<back_off_growth_factor>,<min_back_off>,<max_back_off>".
    /// back_off_growth_factor: f64 >= 1.0
    /// min_back_off: f64 in seconds
    /// max_back_off: f64 in seconds
    #[clap(long, env, verbatim_doc_comment)]
    pub paraswap_rate_limiter: Option<RateLimitingStrategy>,

    #[clap(long, env)]
    pub zeroex_url: Option<String>,

    #[clap(long, env)]
    pub zeroex_api_key: Option<String>,

    /// If solvers should use internal buffers to improve solution quality.
    #[clap(long, env)]
    pub use_internal_buffers: bool,

    /// The Balancer V2 factories to consider for indexing liquidity. Allows
    /// specific pool kinds to be disabled via configuration. Will use all
    /// supported Balancer V2 factory kinds if not specified.
    #[clap(long, env, value_enum, ignore_case = true, use_value_delimiter = true)]
    pub balancer_factories: Option<Vec<BalancerFactoryKind>>,

    /// The list of disabled 1Inch protocols. By default, the `PMM1` protocol
    /// (representing a private market maker) is disabled as it seems to
    /// produce invalid swaps.
    #[clap(long, env, default_value = "PMM1", use_value_delimiter = true)]
    pub disabled_one_inch_protocols: Vec<String>,

    /// The 1Inch REST API URL to use.
    #[structopt(long, env, default_value = "https://api.1inch.exchange/")]
    pub one_inch_url: Url,

    /// Which address should receive the rewards for referring trades to 1Inch.
    #[structopt(long, env)]
    pub one_inch_referrer_address: Option<H160>,

    /// The list of disabled 0x sources.
    #[clap(long, env, use_value_delimiter = true)]
    pub disabled_zeroex_sources: Vec<String>,

    /// Deny list of balancer pool ids.
    #[clap(long, env, use_value_delimiter = true)]
    pub balancer_pool_deny_list: Vec<H256>,

    /// Value of the authorization header for the solver competition post api.
    #[clap(long, env)]
    pub solver_competition_auth: Option<String>,

    /// If liquidity pool fetcher has caching mechanism, this argument defines
    /// how old pool data is allowed to be before updating
    #[clap(
        long,
        env,
        default_value = "30",
        value_parser = duration_from_seconds,
    )]
    pub liquidity_fetcher_max_age_update: Duration,

    /// The number of pools to initially populate the UniswapV3 cache
    #[clap(long, env, default_value = "100")]
    pub max_pools_to_initialize_cache: u64,

    /// Override address of the settlement contract.
    #[clap(long, env)]
    pub settlement_contract_address: Option<H160>,

    /// Override address of the settlement contract.
    #[clap(long, env)]
    pub native_token_address: Option<H160>,

    /// Override address of the balancer vault contract.
    #[clap(long, env)]
    pub balancer_v2_vault_address: Option<H160>,
}

pub fn display_secret_option<T>(
    f: &mut Formatter<'_>,
    name: &str,
    option: &Option<T>,
) -> std::fmt::Result {
    display_option(f, name, &option.as_ref().map(|_| "SECRET"))
}

pub fn display_option(
    f: &mut Formatter<'_>,
    name: &str,
    option: &Option<impl Display>,
) -> std::fmt::Result {
    write!(f, "{name}: ")?;
    match option {
        Some(display) => writeln!(f, "{display}"),
        None => writeln!(f, "None"),
    }
}

pub fn display_list<T>(
    f: &mut Formatter<'_>,
    name: &str,
    iter: impl IntoIterator<Item = T>,
) -> std::fmt::Result
where
    T: Display,
{
    write!(f, "{name}: [")?;
    for (i, t) in iter.into_iter().enumerate() {
        if i != 0 {
            f.write_str(", ")?;
        }
        write!(f, "{t}")?;
    }
    writeln!(f, "]")?;
    Ok(())
}

impl Display for OrderQuotingArguments {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "eip1271_onchain_quote_validity_second: {:?}",
            self.eip1271_onchain_quote_validity_seconds
        )?;
        writeln!(
            f,
            "presign_onchain_quote_validity_second: {:?}",
            self.presign_onchain_quote_validity_seconds
        )?;
        writeln!(f, "fee_discount: {}", self.fee_discount)?;
        writeln!(f, "min_discounted_fee: {}", self.min_discounted_fee)?;
        writeln!(f, "fee_factor: {}", self.fee_factor)?;
        writeln!(
            f,
            "partner_additional_fee_factors: {:?}",
            self.partner_additional_fee_factors
        )?;
        writeln!(f, "cow_fee_factors: {:?}", self.cow_fee_factors)?;
        writeln!(f, "price_estimators: {:?}", self.price_estimators)?;
        display_list(
            f,
            "price_estimation_drivers",
            &self.price_estimation_drivers,
        )?;
        writeln!(
            f,
            "liquidity_order_owners: {:?}",
            self.liquidity_order_owners
        )?;
        Ok(())
    }
}
// We have a custom Display implementation so that we can log the arguments on
// start up without leaking any potentially secret values.
impl Display for Arguments {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", self.ethrpc)?;
        write!(f, "{}", self.current_block)?;
        write!(f, "{}", self.tenderly)?;
        writeln!(f, "log_filter: {}", self.logging.log_filter)?;
        writeln!(
            f,
            "log_stderr_threshold: {}",
            self.logging.log_stderr_threshold
        )?;
        writeln!(f, "node_url: {}", self.node_url)?;
        display_option(f, "chain_id", &self.chain_id)?;
        writeln!(f, "gas_estimators: {:?}", self.gas_estimators)?;
        display_secret_option(f, "blocknative_api_key", &self.blocknative_api_key)?;
        writeln!(f, "base_tokens: {:?}", self.base_tokens)?;
        writeln!(f, "baseline_sources: {:?}", self.baseline_sources)?;
        writeln!(f, "pool_cache_blocks: {}", self.pool_cache_blocks)?;
        writeln!(
            f,
            "pool_cache_maximum_recent_block_age: {}",
            self.pool_cache_maximum_recent_block_age
        )?;
        writeln!(
            f,
            "pool_cache_maximum_retries: {}",
            self.pool_cache_maximum_retries
        )?;
        writeln!(
            f,
            "pool_cache_delay_between_retries_seconds: {:?}",
            self.pool_cache_delay_between_retries_seconds
        )?;
        display_secret_option(f, "paraswap_partner", &self.paraswap_partner)?;
        display_list(f, "disabled_paraswap_dexs", &self.disabled_paraswap_dexs)?;
        display_option(f, "paraswap_rate_limiter", &self.paraswap_rate_limiter)?;
        display_option(f, "zeroex_url", &self.zeroex_url)?;
        display_secret_option(f, "zeroex_api_key", &self.zeroex_api_key)?;
        writeln!(f, "use_internal_buffers: {}", self.use_internal_buffers)?;
        writeln!(f, "balancer_factories: {:?}", self.balancer_factories)?;
        display_list(
            f,
            "disabled_one_inch_protocols",
            &self.disabled_one_inch_protocols,
        )?;
        writeln!(f, "one_inch_url: {}", self.one_inch_url)?;
        display_option(
            f,
            "one_inch_referrer_address",
            &self.one_inch_referrer_address.map(|a| format!("{a:?}")),
        )?;
        display_list(f, "disabled_zeroex_sources", &self.disabled_zeroex_sources)?;
        writeln!(
            f,
            "balancer_pool_deny_list: {:?}",
            self.balancer_pool_deny_list
        )?;
        display_secret_option(f, "solver_competition_auth", &self.solver_competition_auth)?;
        display_option(
            f,
            "settlement_contract_address",
            &self.settlement_contract_address.map(|a| format!("{a:?}")),
        )?;
        display_option(
            f,
            "native_token_address",
            &self.native_token_address.map(|a| format!("{a:?}")),
        )?;
        display_option(
            f,
            "balancer_v2_vault_address",
            &self.balancer_v2_vault_address.map(|a| format!("{a:?}")),
        )?;
        display_list(
            f,
            "custom_univ2_baseline_sources",
            &self.custom_univ2_baseline_sources,
        )?;

        Ok(())
    }
}

impl Display for Driver {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.name, self.url)
    }
}

pub fn parse_unbounded_factor(s: &str) -> Result<f64> {
    let factor = f64::from_str(s)?;
    ensure!(factor.is_finite() && factor >= 0.);
    Ok(factor)
}

pub fn parse_percentage_factor(s: &str) -> Result<f64> {
    let percentage_factor = f64::from_str(s)?;
    ensure!(percentage_factor.is_finite() && (0. ..=1.0).contains(&percentage_factor));
    Ok(percentage_factor)
}

pub fn duration_from_seconds(s: &str) -> Result<Duration, ParseFloatError> {
    Ok(Duration::from_secs_f64(s.parse()?))
}

pub fn wei_from_base_unit(s: &str) -> anyhow::Result<U256> {
    Ok(U256::from_dec_str(s)? * U256::exp10(18))
}

pub fn wei_from_gwei(s: &str) -> anyhow::Result<f64> {
    let in_gwei: f64 = s.parse()?;
    Ok(in_gwei * 1e9)
}

impl FromStr for Driver {
    type Err = anyhow::Error;

    fn from_str(driver: &str) -> Result<Self> {
        let (name, url) = driver
            .split_once('|')
            .context("not enough arguments for driver")?;
        let url: Url = url.parse()?;
        Ok(Driver {
            name: name.to_owned(),
            url,
        })
    }
}

impl FromStr for RateLimitingStrategy {
    type Err = anyhow::Error;

    fn from_str(config: &str) -> Result<Self> {
        let mut parts = config.split(',');
        let back_off_growth_factor = parts.next().context("missing back_off_growth_factor")?;
        let min_back_off = parts.next().context("missing min_back_off")?;
        let max_back_off = parts.next().context("missing max_back_off")?;
        ensure!(
            parts.next().is_none(),
            "extraneous rate limiting parameters"
        );
        let back_off_growth_factor: f64 = back_off_growth_factor
            .parse()
            .context("parsing back_off_growth_factor")?;
        let min_back_off = duration_from_seconds(min_back_off).context("parsing min_back_off")?;
        let max_back_off = duration_from_seconds(max_back_off).context("parsing max_back_off")?;
        Self::try_new(back_off_growth_factor, min_back_off, max_back_off)
    }
}

/// Parses a comma separated list of colon separated values representing fee
/// factors for AppIds.
fn parse_partner_fee_factor(s: &str) -> Result<HashMap<AppId, f64>> {
    let mut res = HashMap::default();
    if s.is_empty() {
        return Ok(res);
    }
    for pair_str in s.split(',') {
        let mut split = pair_str.trim().split(':');
        let key = split
            .next()
            .context("missing AppId")?
            .trim()
            .parse()
            .context("failed to parse address")?;
        let value = split
            .next()
            .context("missing value")?
            .trim()
            .parse::<f64>()
            .context("failed to parse fee factor")?;
        if split.next().is_some() {
            return Err(anyhow!("Invalid pair lengths"));
        }
        res.insert(key, value);
    }
    Ok(res)
}

#[cfg(test)]
mod test {
    use {super::*, maplit::hashmap};
    #[test]
    fn parse_partner_fee_factor_ok() {
        let x = "0x0000000000000000000000000000000000000000000000000000000000000000";
        let y = "0x0101010101010101010101010101010101010101010101010101010101010101";
        // without spaces
        assert_eq!(
            parse_partner_fee_factor(&format!("{x}:0.5,{y}:0.7")).unwrap(),
            hashmap! { AppId([0u8; 32]) => 0.5, AppId([1u8; 32]) => 0.7 }
        );
        // with spaces
        assert_eq!(
            parse_partner_fee_factor(&format!("{x}: 0.5, {y}: 0.7")).unwrap(),
            hashmap! { AppId([0u8; 32]) => 0.5, AppId([1u8; 32]) => 0.7 }
        );
        // whole numbers
        assert_eq!(
            parse_partner_fee_factor(&format!("{x}: 1, {y}: 2")).unwrap(),
            hashmap! { AppId([0u8; 32]) => 1., AppId([1u8; 32]) => 2. }
        );
    }

    #[test]
    fn parse_partner_fee_factor_err() {
        assert!(parse_partner_fee_factor("0x1:0.5,0x2:0.7").is_err());
        assert!(parse_partner_fee_factor("0x12:0.5,0x22:0.7").is_err());
        assert!(parse_partner_fee_factor(
            "0x0000000000000000000000000000000000000000000000000000000000000000:0.5:3"
        )
        .is_err());
        assert!(parse_partner_fee_factor(
            "0x0000000000000000000000000000000000000000000000000000000000000000:word"
        )
        .is_err());
    }

    #[test]
    fn parse_partner_fee_factor_ok_on_empty() {
        assert!(parse_partner_fee_factor("").unwrap().is_empty());
    }

    #[test]
    fn parse_driver() {
        let argument = "name1|http://localhost:8080";
        let driver = Driver::from_str(argument).unwrap();
        let expected = Driver {
            name: "name1".into(),
            url: Url::parse("http://localhost:8080").unwrap(),
        };
        assert_eq!(driver, expected);
    }

    #[test]
    fn parse_drivers_wrong_arguments() {
        // too few arguments
        assert!(Driver::from_str("").is_err());
        assert!(Driver::from_str("name").is_err());

        // broken URL
        assert!(Driver::from_str("name1|sdfsdfds").is_err());

        // too many arguments
        assert!(Driver::from_str("name1|http://localhost:8080|additional_argument").is_err());
    }
}
