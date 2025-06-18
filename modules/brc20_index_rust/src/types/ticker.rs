#[derive(Debug, Clone)]
pub struct Ticker {
    pub ticker: String,
    pub original_ticker: String,
    pub _max_supply: u128,
    pub remaining_supply: u128,
    pub burned_supply: u128,
    pub limit_per_mint: u128,
    pub decimals: u8,
    pub is_self_mint: bool,
    pub deploy_block_height: i32,
    pub deploy_inscription_id: String,
}
