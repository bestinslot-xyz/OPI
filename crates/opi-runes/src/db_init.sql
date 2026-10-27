CREATE TABLE public.runes_id_to_entry (
	id bigserial NOT NULL,
	rune_id text NOT NULL,
	rune_block int4 NOT NULL,
	burned numeric(40) NOT NULL,
	divisibility int4 NOT NULL,
	etching text NOT NULL,
	terms_amount numeric(40) NULL,
	terms_cap numeric(40) NULL,
	terms_height_l int8 NULL,
	terms_height_h int8 NULL,
	terms_offset_l int8 NULL,
	terms_offset_h int8 NULL,
	mints numeric(40) NOT NULL,
	"number" numeric(40) NOT NULL,
	premine numeric(40) NOT NULL,
	rune_name text NOT NULL,
	spacers int8 NOT NULL,
	symbol text NULL,
	"timestamp" timestamptz NOT NULL,
	turbo bool NOT NULL,
	genesis_height int4 NOT NULL,
	last_updated_block_height int4 NOT NULL,
	CONSTRAINT runes_id_to_entry_pk PRIMARY KEY (id)
);
CREATE INDEX runes_id_to_entry_genesis_height_idx ON public.runes_id_to_entry USING btree (genesis_height);
CREATE INDEX runes_id_to_entry_last_updated_block_height_idx ON public.runes_id_to_entry USING btree (last_updated_block_height);
CREATE UNIQUE INDEX runes_id_to_entry_rune_id_idx ON public.runes_id_to_entry USING btree (rune_id);
CREATE UNIQUE INDEX runes_id_to_entry_rune_name_idx ON public.runes_id_to_entry USING btree (rune_name);

CREATE TABLE public.runes_id_to_entry_changes (
	id bigserial NOT NULL,
	rune_id text NOT NULL,
	burned numeric(40) NOT NULL,
	mints numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT runes_id_to_entry_changes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX runes_id_to_entry_changes_rune_id_block_height_idx ON public.runes_id_to_entry_changes USING btree (rune_id, block_height);
CREATE INDEX runes_id_to_entry_changes_block_height_idx ON public.runes_id_to_entry_changes USING btree (block_height);

CREATE TABLE public.runes_outpoint_to_balances (
	id bigserial NOT NULL,
	outpoint text NOT NULL,
	pkscript text NOT NULL,
	wallet_addr text NULL,
	rune_ids text[] NOT NULL,
	balances numeric(40)[] NOT NULL,
	block_height int4 NOT NULL,
	spent bool NOT NULL DEFAULT false,
	spent_block_height int4 NULL,
	CONSTRAINT runes_outpoint_to_balances_pk PRIMARY KEY (id)
);
CREATE INDEX runes_outpoint_to_balances_block_height_idx ON public.runes_outpoint_to_balances USING btree (block_height);
CREATE INDEX runes_outpoint_to_balances_pkscript_idx ON public.runes_outpoint_to_balances USING btree (pkscript);
CREATE INDEX runes_outpoint_to_balances_wallet_addr_idx ON public.runes_outpoint_to_balances USING btree (wallet_addr);
CREATE INDEX runes_outpoint_to_balances_spent_idx ON public.runes_outpoint_to_balances USING btree (spent);
CREATE UNIQUE INDEX runes_outpoint_to_balances_outpoint_idx ON public.runes_outpoint_to_balances USING btree (outpoint);
CREATE INDEX runes_outpoint_to_balances_rune_ids_idx ON public.runes_outpoint_to_balances USING GIN (rune_ids);

CREATE TABLE public.runes_events (
	id bigserial NOT NULL,
	event_type int4 NOT NULL,
	txid text NOT NULL,
	outpoint text NULL,
	pkscript text NULL,
	wallet_addr text NULL,
	rune_id text NOT NULL,
	amount numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT runes_events_pk PRIMARY KEY (id)
);
CREATE INDEX runes_events_block_height_idx ON public.runes_events USING btree (block_height);
CREATE INDEX runes_events_event_type_idx ON public.runes_events USING btree (event_type);
CREATE INDEX runes_events_txid_idx ON public.runes_events USING btree (txid);
CREATE INDEX runes_events_outpoint_idx ON public.runes_events USING btree (outpoint);
CREATE INDEX runes_events_pkscript_idx ON public.runes_events USING btree (pkscript);
CREATE INDEX runes_events_wallet_addr_idx ON public.runes_events USING btree (wallet_addr);
CREATE INDEX runes_events_rune_id_idx ON public.runes_events USING btree (rune_id);

CREATE TABLE public.runes_event_types (
	id bigserial NOT NULL,
	event_type_name text NOT NULL,
	event_type_id int4 NOT NULL,
	CONSTRAINT runes_event_types_pk PRIMARY KEY (id)
);
INSERT INTO public.runes_event_types (event_type_name, event_type_id) VALUES ('input', 0);
INSERT INTO public.runes_event_types (event_type_name, event_type_id) VALUES ('new-allocation', 1);
INSERT INTO public.runes_event_types (event_type_name, event_type_id) VALUES ('mint', 2);
INSERT INTO public.runes_event_types (event_type_name, event_type_id) VALUES ('output', 3);
INSERT INTO public.runes_event_types (event_type_name, event_type_id) VALUES ('burn', 4);

CREATE TABLE public.runes_cumulative_event_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_event_hash text NOT NULL,
	cumulative_event_hash text NOT NULL,
	CONSTRAINT runes_cumulative_event_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX runes_cumulative_event_hashes_block_height_idx ON public.runes_cumulative_event_hashes USING btree (block_height);

CREATE TABLE public.runes_block_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	block_time timestamptz NOT NULL,
	CONSTRAINT runes_block_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX runes_block_hashes_block_height_idx ON public.runes_block_hashes USING btree (block_height);

CREATE TABLE public.runes_indexer_reorg_stats (
	id bigserial NOT NULL,
	reorg_tm int8 NOT NULL,
	old_block_height int4 NOT NULL,
	new_block_height int4 NOT NULL,
	CONSTRAINT runes_indexer_reorg_stats_pk PRIMARY KEY (id)
);

CREATE TABLE public.runes_indexer_work_stats (
	id bigserial NOT NULL,
	main_min_block_height int4 NULL,
	main_max_block_height int4 NULL,
	ord_sql_query_count int4 NULL,
	new_runes_count int4 NULL,
	updated_runes_count int4 NULL,
	new_balances_count int4 NULL,
	removed_balances_count int4 NULL,
	added_entry_history_count int4 NULL,
	added_event_count int4 NULL,
	ord_index_tm int4 NULL,
	ord_sql_tm int4 NULL,
	update_log_tm int4 NULL,
	all_tm int4 NULL,
	ts timestamptz NOT NULL DEFAULT now(),
	CONSTRAINT runes_indexer_work_stats_pk PRIMARY KEY (id)
);

CREATE TABLE public.runes_network_type (
	id bigserial NOT NULL,
	network_type text NOT NULL,
	CONSTRAINT runes_network_type_pk PRIMARY KEY (id)
);

CREATE TABLE public.runes_indexer_version (
	id bigserial NOT NULL,
	indexer_version text NOT NULL,
	db_version int4 NOT NULL,
	CONSTRAINT runes_indexer_version_pk PRIMARY KEY (id)
);
INSERT INTO public.runes_indexer_version (indexer_version, db_version) VALUES ('OPI-runes-alpha V0.4.2', 6);
