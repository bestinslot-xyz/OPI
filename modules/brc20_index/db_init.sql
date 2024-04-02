CREATE TABLE public.brc20_block_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	CONSTRAINT brc20_block_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_block_hashes_block_height_idx ON public.brc20_block_hashes USING btree (block_height);

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
CREATE INDEX brc20_historic_balances_tick_idx ON public.brc20_historic_balances USING btree (tick);
CREATE INDEX brc20_historic_balances_wallet_idx ON public.brc20_historic_balances USING btree (wallet);

CREATE TABLE public.brc20_events (
	id bigserial NOT NULL,
	event_type int4 NOT NULL,
	block_height int4 NOT NULL,
	inscription_id text NOT NULL,
	"event" jsonb NOT NULL,
	CONSTRAINT events_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_events_event_type_inscription_id_idx ON public.brc20_events USING btree (event_type, inscription_id);
CREATE INDEX brc20_events_block_height_idx ON public.brc20_events USING btree (block_height);
CREATE INDEX brc20_events_event_type_idx ON public.brc20_events USING btree (event_type);
CREATE INDEX brc20_events_inscription_id_idx ON public.brc20_events USING btree (inscription_id);

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

CREATE TABLE public.brc20_indexer_version (
	id bigserial NOT NULL,
	indexer_version text NOT NULL,
	db_version int4 NOT NULL,
	event_hash_version int4 NOT NULL,
	CONSTRAINT brc20_indexer_version_pk PRIMARY KEY (id)
);
INSERT INTO public.brc20_indexer_version (indexer_version, db_version, event_hash_version) VALUES ('opi-brc20-full-node v0.4.1', 5, 2);
