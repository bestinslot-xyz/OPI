CREATE TABLE public.bitmap_block_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	CONSTRAINT bitmap_block_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX bitmap_block_hashes_block_height_idx ON public.bitmap_block_hashes USING btree (block_height);

CREATE TABLE public.bitmaps (
	id bigserial NOT NULL,
	inscription_id text NOT NULL,
	bitmap_number int4 NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT bitmaps_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX bitmaps_bitmap_number_idx ON public.bitmaps USING btree (bitmap_number);
CREATE INDEX bitmaps_block_height_idx ON public.bitmaps USING btree (block_height);
CREATE INDEX bitmaps_inscription_id_idx ON public.bitmaps USING btree (inscription_id);

CREATE TABLE public.bitmap_cumulative_event_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_event_hash text NOT NULL,
	cumulative_event_hash text NOT NULL,
	CONSTRAINT bitmap_cumulative_event_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX bitmap_cumulative_event_hashes_block_height_idx ON public.bitmap_cumulative_event_hashes USING btree (block_height);

CREATE TABLE public.bitmap_indexer_version (
	id bigserial NOT NULL,
	indexer_version text NOT NULL,
	db_version int4 NOT NULL,
	CONSTRAINT bitmap_indexer_version_pk PRIMARY KEY (id)
);
INSERT INTO public.bitmap_indexer_version (indexer_version, db_version) VALUES ('opi-bitmap-full-node v0.3.0', 3);
