//! The context window as the loop accounts for it (chat/tokens.go, WP53): the meter and the budget, and
//! the offline `o200k_base` token counter behind them.

pub(crate) mod meter;
/// The `o200k_base` token counter behind the meter (WP53).
pub(crate) mod tokens;
