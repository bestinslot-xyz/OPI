CREATE TABLE public.pow20_block_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	CONSTRAINT pow20_block_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX pow20_block_hashes_block_height_idx ON public.pow20_block_hashes USING btree (block_height);

CREATE TABLE public.pow20_historic_balances (
	id bigserial NOT NULL,
	pkscript text NOT NULL,
	wallet text NULL,
	tick varchar(4) NOT NULL,
	overall_balance numeric(40) NOT NULL,
	available_balance numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	event_id int8 NOT NULL,
	CONSTRAINT pow20_historic_balances_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX pow20_historic_balances_event_id_idx ON public.pow20_historic_balances USING btree (event_id);
CREATE INDEX pow20_historic_balances_block_height_idx ON public.pow20_historic_balances USING btree (block_height);
CREATE INDEX pow20_historic_balances_pkscript_idx ON public.pow20_historic_balances USING btree (pkscript);
CREATE INDEX pow20_historic_balances_pkscript_tick_block_height_idx ON public.pow20_historic_balances USING btree (pkscript, tick, block_height);
CREATE INDEX pow20_historic_balances_tick_idx ON public.pow20_historic_balances USING btree (tick);
CREATE INDEX pow20_historic_balances_wallet_idx ON public.pow20_historic_balances USING btree (wallet);

CREATE TABLE public.pow20_events (
	id bigserial NOT NULL,
	event_type int4 NOT NULL,
	block_height int4 NOT NULL,
	inscription_id text NOT NULL,
	"event" jsonb NOT NULL,
	CONSTRAINT pow20_events_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX pow20_events_event_type_inscription_id_idx ON public.pow20_events USING btree (event_type, inscription_id);
CREATE INDEX pow20_events_block_height_idx ON public.pow20_events USING btree (block_height);
CREATE INDEX pow20_events_event_type_idx ON public.pow20_events USING btree (event_type);
CREATE INDEX pow20_events_inscription_id_idx ON public.pow20_events USING btree (inscription_id);


CREATE TABLE public.pow20_tickers (
	id bigserial NOT NULL,
	tick varchar(4) NOT NULL,
	max_supply numeric(40) NOT NULL,
	decimals int4 NOT NULL,
	difficulty int4 NOT NULL,
	starting_block_height int4 NOT NULL,
	limit_per_mint numeric(40) NOT NULL,
	remaining_supply numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT pow20_tickers_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX pow20_tickers_tick_idx ON public.pow20_tickers USING btree (tick);
CREATE INDEX pow20_tickers_starting_block_height_idx ON public.pow20_tickers USING btree (starting_block_height);

CREATE TABLE public.pow20_cumulative_event_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_event_hash text NOT NULL,
	cumulative_event_hash text NOT NULL,
	CONSTRAINT pow20_cumulative_event_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX pow20_cumulative_event_hashes_block_height_idx ON public.pow20_cumulative_event_hashes USING btree (block_height);

CREATE TABLE public.pow20_event_types (
	id bigserial NOT NULL,
	event_type_name text NOT NULL,
	event_type_id int4 NOT NULL,
	CONSTRAINT pow20_event_types_pk PRIMARY KEY (id)
);
INSERT INTO public.pow20_event_types (event_type_name, event_type_id) VALUES ('deploy-inscribe', 0);
INSERT INTO public.pow20_event_types (event_type_name, event_type_id) VALUES ('mint-inscribe', 1);
INSERT INTO public.pow20_event_types (event_type_name, event_type_id) VALUES ('transfer-inscribe', 2);
INSERT INTO public.pow20_event_types (event_type_name, event_type_id) VALUES ('transfer-transfer', 3);

CREATE TABLE public.pow20_indexer_version (
	id bigserial NOT NULL,
	indexer_version text NOT NULL,
	db_version int4 NOT NULL,
	CONSTRAINT pow20_indexer_version_pk PRIMARY KEY (id)
);
INSERT INTO public.pow20_indexer_version (indexer_version, db_version) VALUES ('opi-pow20-full-node v0.3.0', 3);
