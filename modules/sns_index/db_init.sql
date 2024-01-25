CREATE TABLE public.sns_block_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	CONSTRAINT sns_block_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX sns_block_hashes_block_height_idx ON public.sns_block_hashes USING btree (block_height);

CREATE TABLE public.sns_names (
	id bigserial NOT NULL,
	inscription_id text NOT NULL,
	inscription_number int4 NOT NULL,
	"name" text NOT NULL,
	domain text NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT sns_names_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX sns_names_name_idx ON public.sns_names USING btree ("name");
CREATE INDEX sns_names_domain_idx ON public.sns_names USING btree (domain);
CREATE INDEX sns_names_block_height_idx ON public.sns_names USING btree (block_height);
CREATE INDEX sns_names_inscription_id_idx ON public.sns_names USING btree (inscription_id);
CREATE INDEX sns_names_inscription_number_idx ON public.sns_names USING btree (inscription_number);

CREATE TABLE public.sns_namespaces (
	id bigserial NOT NULL,
	inscription_id text NOT NULL,
	inscription_number int4 NOT NULL,
	"namespace" text NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT sns_namespaces_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX sns_namespaces_namespace_idx ON public.sns_namespaces USING btree ("namespace");
CREATE INDEX sns_namespaces_block_height_idx ON public.sns_namespaces USING btree (block_height);
CREATE INDEX sns_namespaces_inscription_id_idx ON public.sns_namespaces USING btree (inscription_id);
CREATE INDEX sns_namespaces_inscription_number_idx ON public.sns_namespaces USING btree (inscription_number);

CREATE TABLE public.sns_names_cumulative_event_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_event_hash text NOT NULL,
	cumulative_event_hash text NOT NULL,
	CONSTRAINT sns_names_cumulative_event_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX sns_names_cumulative_event_hashes_block_height_idx ON public.sns_names_cumulative_event_hashes USING btree (block_height);

CREATE TABLE public.sns_names_indexer_version (
	id bigserial NOT NULL,
	indexer_version text NOT NULL,
	db_version int4 NOT NULL,
	CONSTRAINT sns_names_indexer_version_pk PRIMARY KEY (id)
);
INSERT INTO public.sns_names_indexer_version (indexer_version, db_version) VALUES ('opi-sns-names-open-source v0.3.0', 3);
