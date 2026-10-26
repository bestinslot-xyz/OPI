use brc20_index::config::Brc20IndexerConfig;

/// The destructuring lists every field without `..`, so it stops compiling
/// when a field is added, including a BRC2.0 on/off switch.
#[test]
fn config_has_no_prog_toggle() {
    let Brc20IndexerConfig {
        light_client_mode: _,
        save_logs: _,
        db_host: _,
        db_port: _,
        db_user: _,
        db_password: _,
        db_database: _,
        db_ssl: _,
        opi_db_url: _,
        report_to_indexer: _,
        report_all_blocks: _,
        report_url: _,
        report_retries: _,
        report_name: _,
        network_type: _,
        network_type_string: _,
        first_inscription_height: _,
        first_brc20_height: _,
        first_brc20_prog_phase_one_height: _,
        first_brc20_prog_all_tickers_height: _,
        first_brc20_prog_prague_height: _,
        self_mint_activation_height: _,
        brc20_swap_refund_activation_height: _,
        brc20_swap_refund_address: _,
        brc20_swap_module_pkscript: _,
        brc20_prog_rpc_url: _,
        brc20_prog_rpc_user: _,
        brc20_prog_rpc_password: _,
        brc20_prog_bitcoin_rpc_proxy_server_enabled: _,
        brc20_prog_bitcoin_rpc_proxy_server_addr: _,
        bitcoin_rpc_cache_enabled: _,
        bitcoin_rpc_url: _,
        height_limit: _,
    } = Brc20IndexerConfig::default();
}
