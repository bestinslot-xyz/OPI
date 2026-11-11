-- One row per bound inscription, at its current location. `content_type` is
-- the envelope's content type as inscribed (lossy UTF-8, NUL bytes dropped)
-- and `media_type` the same lowercased with parameters split off, NULL when
-- the envelope has none. The location is the output's `txid` (the 32 bytes
-- its hex spells) and `vout`, the sat's `offset` in it and the output's raw
-- `pkscript`; addresses derive from `pkscript` and the network. Cursed
-- inscriptions have a negative `number`. `last_event_id` is the
-- inscriptions_events row of the inscription's latest creation or transfer.
CREATE TABLE public.inscriptions_entries (
	sequence_number int8 NOT NULL,
	"number" int8 NOT NULL,
	"offset" int8 NOT NULL,
	last_event_id int8 NOT NULL,
	charms int4 NOT NULL,
	vout int4 NOT NULL,
	genesis_height int4 NOT NULL,
	last_moved_height int4 NOT NULL,
	inscription_id text NOT NULL,
	content_type text NULL,
	media_type text NULL,
	txid bytea NOT NULL,
	pkscript bytea NOT NULL,
	CONSTRAINT inscriptions_entries_pk PRIMARY KEY (sequence_number)
);
CREATE UNIQUE INDEX inscriptions_entries_inscription_id_idx ON public.inscriptions_entries USING btree (inscription_id);
CREATE UNIQUE INDEX inscriptions_entries_number_idx ON public.inscriptions_entries USING btree ("number");
CREATE INDEX inscriptions_entries_outpoint_idx ON public.inscriptions_entries USING btree (txid, vout);
CREATE INDEX inscriptions_entries_pkscript_seq_idx ON public.inscriptions_entries USING btree (pkscript, sequence_number);

-- One row per creation and transfer, holding the location the inscription
-- moved to. `id` is the block height in its high 32 bits and the move's
-- position in the block in the low 32, so a block's events are the id range
-- [height << 32, (height + 1) << 32) and ids ascend in event order.
-- `prev_id` is the inscription's previous event, NULL for a creation: its
-- location and height are what a transfer moved the inscription from, which a
-- rollback puts back. `txid` is the outpoint's txid as the 32 bytes its hex
-- spells, `pkscript` the raw script; addresses derive from `pkscript` and the
-- network. OPI_PRUNE keeps every event.
CREATE TABLE public.inscriptions_events (
	id int8 NOT NULL,
	sequence_number int8 NOT NULL,
	prev_id int8 NULL,
	"offset" int8 NOT NULL,
	vout int4 NOT NULL,
	txid bytea NOT NULL,
	pkscript bytea NOT NULL,
	CONSTRAINT inscriptions_events_pk PRIMARY KEY (id)
);
-- an inscription's history, and the event after one a wallet received
CREATE INDEX inscriptions_events_sequence_number_id_idx ON public.inscriptions_events USING btree (sequence_number, id);
-- what a wallet received
CREATE INDEX inscriptions_events_pkscript_id_idx ON public.inscriptions_events USING btree (pkscript, id);

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
INSERT INTO public.inscriptions_indexer_version (indexer_version, db_version) VALUES ('opi-inscriptions v2.0.0', 2);
