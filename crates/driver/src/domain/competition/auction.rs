use {
    crate::{
        domain::{
            competition::{self, solution},
            eth,
        },
        infra::time,
    },
    std::{num::ParseIntError, str::FromStr},
    thiserror::Error,
};

/// An auction is a set of orders that can be solved. The solvers calculate
/// [`super::solution::Solution`]s by picking subsets of these orders and
/// solving them.
#[derive(Debug)]
pub struct Auction {
    // TODO Make this non-optional and simplify things
    /// [`None`] if the auction is used for quoting, [`Some`] if the auction is
    /// used for competition.
    pub id: Option<Id>,
    pub tokens: Vec<Token>,
    pub orders: Vec<competition::Order>,
    pub gas_price: eth::EffectiveGasPrice,
    pub deadline: Deadline,
}

#[derive(Debug)]
pub struct Token {
    pub decimals: Option<u8>,
    pub symbol: Option<String>,
    pub address: eth::TokenAddress,
    pub price: Option<Price>,
    /// The balance of this token available in our settlement contract.
    pub available_balance: eth::U256,
    /// Is this token well-known and trusted by the protocol?
    pub trusted: bool,
}

/// The price of a token in wei. This represents how much wei is needed to buy
/// 10**18 of another token.
#[derive(Debug, Clone, Copy)]
pub struct Price(pub eth::Ether);

impl From<Price> for eth::U256 {
    fn from(value: Price) -> Self {
        value.0.into()
    }
}

impl From<eth::U256> for Price {
    fn from(value: eth::U256) -> Self {
        Self(value.into())
    }
}

/// Each auction has a deadline, limiting the maximum time that can be allocated
/// to solving the auction.
#[derive(Debug, Default, Clone, Copy)]
pub struct Deadline(chrono::DateTime<chrono::Utc>);

impl Deadline {
    /// Computes the timeout for solving an auction.
    pub fn timeout(self, now: time::Now) -> Result<solution::SolverTimeout, DeadlineExceeded> {
        solution::SolverTimeout::new(self.into(), Self::time_buffer(), now).ok_or(DeadlineExceeded)
    }

    pub fn time_buffer() -> chrono::Duration {
        chrono::Duration::seconds(1)
    }
}

impl From<chrono::DateTime<chrono::Utc>> for Deadline {
    fn from(value: chrono::DateTime<chrono::Utc>) -> Self {
        Self(value)
    }
}

impl From<Deadline> for chrono::DateTime<chrono::Utc> {
    fn from(value: Deadline) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Id(pub u64);

impl From<u64> for Id {
    fn from(inner: u64) -> Self {
        Self(inner)
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Id {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        FromStr::from_str(s).map(Self)
    }
}

#[derive(Debug, Error)]
#[error("the solution deadline has been exceeded")]
pub struct DeadlineExceeded;
