//! Pins opi-brc20's copies of brc20-prog's chain ids to brc20-prog's own,
//! which it derives from BITCOIN_RPC_NETWORK. One test function, since it
//! sets the process environment.

use brc20_prog::Brc20ProgConfig;
use opi_brc20::config::{BRC20_PROG_CHAIN_ID_MAINNET, BRC20_PROG_CHAIN_ID_TESTNETS};

#[test]
fn chain_ids_match_brc20_prog() {
    for (network, chain_id) in [
        ("bitcoin", BRC20_PROG_CHAIN_ID_MAINNET),
        ("signet", BRC20_PROG_CHAIN_ID_TESTNETS),
    ] {
        // SAFETY: this binary's only test; no other thread reads the environment.
        unsafe { std::env::set_var("BITCOIN_RPC_NETWORK", network) };
        assert_eq!(Brc20ProgConfig::from_env().chain_id, chain_id, "{network}");
    }
}
