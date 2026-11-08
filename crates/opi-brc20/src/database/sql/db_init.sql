CREATE TABLE public.brc20_block_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	block_time int8,
	CONSTRAINT brc20_block_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_block_hashes_block_height_idx ON public.brc20_block_hashes USING btree (block_height);
CREATE INDEX IF NOT EXISTS brc20_block_hashes_block_hash_idx ON public.brc20_block_hashes USING btree (block_hash);

CREATE TABLE public.brc20_historic_balances (
	id bigserial NOT NULL,
	pkscript text NOT NULL,
	wallet text NULL,
	tick text NOT NULL,
	overall_balance numeric(40) NOT NULL,
	available_balance numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	event_id int8 NOT NULL,
	CONSTRAINT brc20_historic_balances_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_historic_balances_event_id_idx ON public.brc20_historic_balances USING btree (event_id);
CREATE INDEX brc20_historic_balances_block_height_idx ON public.brc20_historic_balances USING btree (block_height);
CREATE INDEX brc20_historic_balances_pkscript_idx ON public.brc20_historic_balances USING btree (pkscript);
CREATE INDEX brc20_historic_balances_pkscript_tick_block_height_idx ON public.brc20_historic_balances USING btree (pkscript, tick, block_height);
CREATE INDEX IF NOT EXISTS brc20_historic_balances_tick_pkscript_block_height_id_idx ON public.brc20_historic_balances USING btree (tick, pkscript, block_height DESC, id DESC) INCLUDE (wallet, overall_balance, available_balance);
CREATE INDEX brc20_historic_balances_wallet_idx ON public.brc20_historic_balances USING btree (wallet);

CREATE TABLE public.brc20_events (
	id bigserial NOT NULL,
	event_type int4 NOT NULL,
	block_height int4 NOT NULL,
	inscription_id text NOT NULL,
	inscription_number int4 NOT NULL,
	old_satpoint text,
	new_satpoint text NOT NULL,
	txid text NOT NULL,
	"event" jsonb NOT NULL,
	CONSTRAINT events_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_events_event_type_inscription_id_idx ON public.brc20_events USING btree (event_type, inscription_id);
CREATE INDEX brc20_events_block_height_idx ON public.brc20_events USING btree (block_height);
CREATE INDEX brc20_events_event_type_idx ON public.brc20_events USING btree (event_type);
CREATE INDEX brc20_events_inscription_id_idx ON public.brc20_events USING btree (inscription_id);

CREATE TABLE public.brc20_light_events (
	id bigserial NOT NULL,
	event_type int4 NOT NULL,
	block_height int4 NOT NULL,
	inscription_id text NOT NULL,
	"event" jsonb NOT NULL,
	CONSTRAINT light_events_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_light_events_event_type_inscription_id_idx ON public.brc20_light_events USING btree (event_type, inscription_id);
CREATE INDEX brc20_light_events_block_height_idx ON public.brc20_light_events USING btree (block_height);
CREATE INDEX brc20_light_events_event_type_idx ON public.brc20_light_events USING btree (event_type);
CREATE INDEX brc20_light_events_inscription_id_idx ON public.brc20_light_events USING btree (inscription_id);

CREATE TABLE IF NOT EXISTS public.evm_addresses (
	id serial PRIMARY KEY,
	evm_address text NOT NULL UNIQUE,
	btc_address text NOT NULL UNIQUE
);

CREATE TABLE public.brc20_tickers (
	id bigserial NOT NULL,
	original_tick text NOT NULL,
	tick text NOT NULL,
	max_supply numeric(40) NOT NULL,
	decimals int4 NOT NULL,
	limit_per_mint numeric(40) NOT NULL,
	remaining_supply numeric(40) NOT NULL,
	burned_supply numeric(40) NOT NULL DEFAULT 0,
	is_self_mint boolean NOT NULL,
	deploy_inscription_id text NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_tickers_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_tickers_original_tick_idx ON public.brc20_tickers USING btree (original_tick);
CREATE UNIQUE INDEX brc20_tickers_tick_idx ON public.brc20_tickers USING btree (tick);

CREATE TABLE public.brc20_cumulative_event_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_event_hash text NOT NULL,
	cumulative_event_hash text NOT NULL,
	block_trace_hash text NOT NULL,
	cumulative_trace_hash text NOT NULL,
	CONSTRAINT brc20_cumulative_event_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_cumulative_event_hashes_block_height_idx ON public.brc20_cumulative_event_hashes USING btree (block_height);

CREATE TABLE public.brc20_event_types (
	id bigserial NOT NULL,
	event_type_name text NOT NULL,
	event_type_id int4 NOT NULL,
	CONSTRAINT brc20_event_types_pk PRIMARY KEY (id)
);
INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('deploy-inscribe', 0);
INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('mint-inscribe', 1);
INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('transfer-inscribe', 2);
INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('transfer-transfer', 3);

INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('brc20prog-deploy-inscribe', 4);
INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('brc20prog-deploy-transfer', 5);

INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('brc20prog-call-inscribe', 6);
INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('brc20prog-call-transfer', 7);

INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('brc20prog-withdraw-inscribe', 8);
INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('brc20prog-withdraw-transfer', 9);

INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('brc20prog-transact-inscribe', 10);
INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('brc20prog-transact-transfer', 11);

INSERT INTO public.brc20_event_types (event_type_name, event_type_id) VALUES ('predeploy-inscribe', 12);

CREATE TABLE public.brc20_indexer_version (
	id bigserial NOT NULL,
	indexer_version text NOT NULL,
	db_version int4 NOT NULL,
	event_hash_version int4 NOT NULL,
	CONSTRAINT brc20_indexer_version_pk PRIMARY KEY (id)
);
INSERT INTO public.brc20_indexer_version (indexer_version, db_version, event_hash_version) VALUES ('opi-brc20-rs-node v2.0.0', 7, 3);

CREATE TABLE public.brc20_current_balances (
	id bigserial NOT NULL,
	pkscript text NOT NULL,
	wallet text NULL,
	tick text NOT NULL,
	overall_balance numeric(40) NOT NULL,
	available_balance numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_current_balances_pk PRIMARY KEY (id)
);

CREATE UNIQUE INDEX brc20_current_balances_pkscript_tick_idx ON public.brc20_current_balances USING btree (pkscript, tick);
CREATE INDEX brc20_current_balances_block_height_idx ON public.brc20_current_balances USING btree (block_height);
CREATE INDEX brc20_current_balances_pkscript_idx ON public.brc20_current_balances USING btree (pkscript);
CREATE INDEX brc20_current_balances_tick_idx ON public.brc20_current_balances USING btree (tick);
CREATE INDEX brc20_current_balances_wallet_idx ON public.brc20_current_balances USING btree (wallet);

CREATE TABLE public.brc20_unused_txes (
	id bigserial NOT NULL,
	inscription_id text NOT NULL,
	tick text NOT NULL,
	amount numeric(40) NOT NULL,
	current_holder_pkscript text NOT NULL,
	current_holder_wallet text NULL,
	event_id int8 NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_unused_txes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_unused_txes_inscription_id_idx ON public.brc20_unused_txes USING btree (inscription_id);
CREATE INDEX brc20_unused_txes_tick_idx ON public.brc20_unused_txes USING btree (tick);
CREATE INDEX brc20_unused_txes_pkscript_idx ON public.brc20_unused_txes USING btree (current_holder_pkscript);
CREATE INDEX brc20_unused_txes_wallet_idx ON public.brc20_unused_txes USING btree (current_holder_wallet);

CREATE TABLE public.brc20_bitcoin_rpc_result_cache (
	id bigserial NOT NULL,
	method text NOT NULL,
	request jsonb NOT NULL,
	response jsonb NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_bitcoin_rpc_result_cache_pk PRIMARY KEY (id)
);
CREATE INDEX brc20_bitcoin_rpc_result_cache_request_idx ON public.brc20_bitcoin_rpc_result_cache USING btree (request);
CREATE INDEX brc20_bitcoin_rpc_result_cache_block_height_idx ON public.brc20_bitcoin_rpc_result_cache USING btree (block_height);

CREATE TABLE public.brc20_logs (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	log_data jsonb NOT NULL,
	CONSTRAINT brc20_logs_pk PRIMARY KEY (id)
);

CREATE INDEX brc20_logs_block_height ON public.brc20_logs (block_height DESC);

CREATE TABLE IF NOT EXISTS public.brc20_prog_tickers (
	contract_address text NOT NULL,
	ticker text NOT NULL,
	ticker_hash text NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_prog_tickers_pk PRIMARY KEY (contract_address)
);
CREATE INDEX IF NOT EXISTS brc20_prog_tickers_ticker_idx ON public.brc20_prog_tickers USING btree (ticker);
CREATE INDEX IF NOT EXISTS brc20_prog_tickers_block_height_idx ON public.brc20_prog_tickers USING btree (block_height);

CREATE TABLE IF NOT EXISTS public.brc20_prog_current_balances (
	wallet text NOT NULL,
	ticker text NOT NULL,
	amount numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_prog_current_balances_pk PRIMARY KEY (wallet, ticker)
);
CREATE INDEX IF NOT EXISTS brc20_prog_current_balances_ticker_idx ON public.brc20_prog_current_balances USING btree (ticker);
CREATE INDEX IF NOT EXISTS brc20_prog_current_balances_block_height_idx ON public.brc20_prog_current_balances USING btree (block_height);

CREATE TABLE IF NOT EXISTS public.brc20_prog_historical_balances (
	wallet text NOT NULL,
	ticker text NOT NULL,
	amount numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_prog_historical_balances_pk PRIMARY KEY (wallet, ticker, block_height)
);
CREATE INDEX IF NOT EXISTS brc20_prog_historical_balances_block_height_idx ON public.brc20_prog_historical_balances USING btree (block_height);
CREATE INDEX IF NOT EXISTS brc20_prog_historical_balances_ticker_wallet_block_height_idx ON public.brc20_prog_historical_balances USING btree (ticker, wallet, block_height DESC) INCLUDE (amount);

CREATE TABLE IF NOT EXISTS public.brc20_ticker_holders (
	tick text NOT NULL,
	holders int8 NOT NULL DEFAULT 0,
	prog_holders int8 NOT NULL DEFAULT 0,
	CONSTRAINT brc20_ticker_holders_pk PRIMARY KEY (tick)
);
CREATE INDEX IF NOT EXISTS brc20_current_balances_tick_holder_idx ON public.brc20_current_balances USING btree (tick, overall_balance DESC) WHERE overall_balance > 0 AND pkscript <> '6a09425243323050524f47';
CREATE INDEX IF NOT EXISTS brc20_prog_current_balances_ticker_positive_idx ON public.brc20_prog_current_balances USING btree (ticker, amount DESC) WHERE amount > 0;
CREATE INDEX IF NOT EXISTS brc20_tickers_tick_pattern_idx ON public.brc20_tickers USING btree (tick text_pattern_ops);

CREATE INDEX IF NOT EXISTS brc20_events_ticker_id_idx ON public.brc20_events USING btree ((("event" ->> 'tick'::text)), id);
CREATE INDEX IF NOT EXISTS brc20_events_parties_idx ON public.brc20_events USING gin ((array_remove(ARRAY[nullif(("event" ->> 'deployer_pkScript'), ''), nullif(("event" ->> 'minted_pkScript'), ''), nullif(("event" ->> 'source_pkScript'), ''), nullif(("event" ->> 'spent_pkScript'), ''), nullif(("event" ->> 'predeployer_pkScript'), ''), nullif(("event" ->> 'deployer_wallet'), ''), nullif(("event" ->> 'minted_wallet'), ''), nullif(("event" ->> 'source_wallet'), ''), nullif(("event" ->> 'spent_wallet'), ''), nullif(("event" ->> 'predeployer_wallet'), '')], NULL)));
CREATE INDEX IF NOT EXISTS brc20_light_events_ticker_id_idx ON public.brc20_light_events USING btree ((("event" ->> 'tick'::text)), id);
CREATE INDEX IF NOT EXISTS brc20_light_events_parties_idx ON public.brc20_light_events USING gin ((array_remove(ARRAY[nullif(("event" ->> 'deployer_pkScript'), ''), nullif(("event" ->> 'minted_pkScript'), ''), nullif(("event" ->> 'source_pkScript'), ''), nullif(("event" ->> 'spent_pkScript'), ''), nullif(("event" ->> 'predeployer_pkScript'), ''), nullif(("event" ->> 'deployer_wallet'), ''), nullif(("event" ->> 'minted_wallet'), ''), nullif(("event" ->> 'source_wallet'), ''), nullif(("event" ->> 'spent_wallet'), ''), nullif(("event" ->> 'predeployer_wallet'), '')], NULL)));

CREATE TABLE IF NOT EXISTS public.brc20_extras_verified (
	id int4 NOT NULL DEFAULT 1,
	verified_at timestamptz NOT NULL DEFAULT now(),
	CONSTRAINT brc20_extras_verified_pk PRIMARY KEY (id),
	CONSTRAINT brc20_extras_verified_one_row CHECK (id = 1)
);
INSERT INTO public.brc20_extras_verified (id) VALUES (1) ON CONFLICT DO NOTHING;

