-- One row per bound inscription, at its current location. `content_type` is
-- the envelope's content type as inscribed (lossy UTF-8, NUL bytes dropped)
-- and `media_type` the same lowercased with parameters split off, NULL when
-- the envelope has none. `pkscript` is hex; `address` is NULL for a script
-- without an address form. Cursed inscriptions have a negative `number`.
CREATE TABLE public.inscriptions_entries (
	sequence_number int8 NOT NULL,
	inscription_id text NOT NULL,
	"number" int8 NOT NULL,
	content_type text NULL,
	media_type text NULL,
	charms int4 NOT NULL,
	outpoint text NOT NULL,
	"offset" int8 NOT NULL,
	pkscript text NOT NULL,
	address text NULL,
	genesis_height int4 NOT NULL,
	last_moved_height int4 NOT NULL,
	CONSTRAINT inscriptions_entries_pk PRIMARY KEY (sequence_number)
);
CREATE UNIQUE INDEX inscriptions_entries_inscription_id_idx ON public.inscriptions_entries USING btree (inscription_id);
CREATE UNIQUE INDEX inscriptions_entries_number_idx ON public.inscriptions_entries USING btree ("number");
CREATE INDEX inscriptions_entries_outpoint_idx ON public.inscriptions_entries USING btree (outpoint);
CREATE INDEX inscriptions_entries_address_seq_idx ON public.inscriptions_entries USING btree (address, sequence_number);
CREATE INDEX inscriptions_entries_pkscript_seq_idx ON public.inscriptions_entries USING btree (pkscript, sequence_number);
CREATE INDEX inscriptions_entries_genesis_height_idx ON public.inscriptions_entries USING btree (genesis_height);

-- One row per creation and transfer, ids ascending in event order. A
-- creation has the old_* columns NULL; a transfer holds the location and
-- `last_moved_height` the inscription had before it, which a rollback puts
-- back. Rows below the OPI_PRUNE window are deleted.
CREATE TABLE public.inscriptions_events (
	id int8 NOT NULL,
	block_height int4 NOT NULL,
	sequence_number int8 NOT NULL,
	inscription_id text NOT NULL,
	old_outpoint text NULL,
	old_offset int8 NULL,
	old_pkscript text NULL,
	old_address text NULL,
	old_last_moved_height int4 NULL,
	new_outpoint text NOT NULL,
	new_offset int8 NOT NULL,
	new_pkscript text NOT NULL,
	new_address text NULL,
	CONSTRAINT inscriptions_events_pk PRIMARY KEY (id)
);
CREATE INDEX inscriptions_events_block_height_idx ON public.inscriptions_events USING btree (block_height);

CREATE TABLE public.inscriptions_block_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	block_time timestamptz NOT NULL,
	CONSTRAINT inscriptions_block_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX inscriptions_block_hashes_block_height_idx ON public.inscriptions_block_hashes USING btree (block_height);

CREATE TABLE public.inscriptions_network_type (
	id bigserial NOT NULL,
	network_type text NOT NULL,
	CONSTRAINT inscriptions_network_type_pk PRIMARY KEY (id)
);

CREATE TABLE public.inscriptions_indexer_version (
	id bigserial NOT NULL,
	indexer_version text NOT NULL,
	db_version int4 NOT NULL,
	CONSTRAINT inscriptions_indexer_version_pk PRIMARY KEY (id)
);
INSERT INTO public.inscriptions_indexer_version (indexer_version, db_version) VALUES ('opi-inscriptions v1.0.0', 1);
