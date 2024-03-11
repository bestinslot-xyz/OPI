CREATE TABLE public.ord_transfers (
	id bigserial NOT NULL,
	inscription_id text NOT NULL,
	block_height int4 NOT NULL,
	old_satpoint text NULL,
	new_satpoint text NOT NULL,
	new_pkscript text NOT NULL,
	new_wallet text NULL,
	sent_as_fee bool NOT NULL,
	new_output_value int8 NOT NULL,
	CONSTRAINT ord_transfers_pk PRIMARY KEY (id)
);
CREATE INDEX ord_transfers_block_height_idx ON public.ord_transfers USING btree (block_height);
CREATE INDEX ord_transfers_inscription_id_idx ON public.ord_transfers USING btree (inscription_id);

CREATE TABLE public.ord_number_to_id (
	id bigserial NOT NULL,
	inscription_number int8 NOT NULL,
	inscription_id text NOT NULL,
	cursed_for_brc20 bool NOT NULL,
	parent_id text NULL,
	block_height int4 NOT NULL,
	CONSTRAINT ord_number_to_id_pk PRIMARY KEY (id)
);
CREATE INDEX ord_number_to_id_block_height_idx ON public.ord_number_to_id USING btree (block_height);
CREATE UNIQUE INDEX ord_number_to_id_inscription_id_idx ON public.ord_number_to_id USING btree (inscription_id);
CREATE UNIQUE INDEX ord_number_to_id_inscription_number_idx ON public.ord_number_to_id USING btree (inscription_number);

CREATE TABLE public.ord_content (
	id bigserial NOT NULL,
	inscription_id text NOT NULL,
	"content" jsonb NULL,
	text_content text NULL,
	content_type text NOT NULL,
	metaprotocol text NULL,
	block_height int4 NOT NULL,
	CONSTRAINT ord_content_pk PRIMARY KEY (id)
);
CREATE INDEX ord_content_block_height_idx ON public.ord_content USING btree (block_height);
CREATE UNIQUE INDEX ord_content_inscription_id_idx ON public.ord_content USING btree (inscription_id);

CREATE TABLE public.block_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	CONSTRAINT block_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX block_hashes_block_height_idx ON public.block_hashes USING btree (block_height);

CREATE TABLE public.ord_indexer_reorg_stats (
	id bigserial NOT NULL,
	reorg_tm int8 NOT NULL,
	old_block_height int4 NOT NULL,
	new_block_height int4 NOT NULL,
	CONSTRAINT ord_indexer_reorg_stats_pk PRIMARY KEY (id)
);

CREATE TABLE public.ord_indexer_work_stats (
	id bigserial NOT NULL,
	main_min_block_height int4 NULL,
	main_max_block_height int4 NULL,
	ord_sql_query_count int4 NULL,
	new_inscription_count int4 NULL,
	transfer_count int4 NULL,
	ord_index_tm int4 NULL,
	ord_sql_tm int4 NULL,
	update_log_tm int4 NULL,
	all_tm int4 NULL,
	ts timestamptz NOT NULL DEFAULT now(),
	CONSTRAINT ord_indexer_work_stats_pk PRIMARY KEY (id)
);

CREATE TABLE public.ord_network_type (
	id bigserial NOT NULL,
	network_type text NOT NULL,
	CONSTRAINT ord_network_type_pk PRIMARY KEY (id)
);

CREATE TABLE public.ord_transfer_counts (
	id bigserial NOT NULL,
	event_type text NOT NULL,
	max_transfer_cnt int4 NOT NULL,
	CONSTRAINT ord_transfer_counts_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX ord_transfer_counts_event_type_idx ON public.ord_transfer_counts USING btree (event_type);

CREATE TABLE public.ord_indexer_version (
	id bigserial NOT NULL,
	indexer_version text NOT NULL,
	db_version int4 NOT NULL,
	CONSTRAINT ord_indexer_version_pk PRIMARY KEY (id)
);
INSERT INTO public.ord_indexer_version (indexer_version, db_version) VALUES ('OPI V0.4.0', 6);
