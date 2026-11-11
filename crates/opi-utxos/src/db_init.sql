-- The unspent outputs at the module's height, as bitcoind's UTXO set holds
-- them. `txid` is the 32 bytes its hex spells, `pkscript` the raw script;
-- addresses derive from `pkscript` and the network. `height` is the block
-- that created the output.
CREATE TABLE public.utxos (
	txid bytea NOT NULL,
	vout int4 NOT NULL,
	value int8 NOT NULL,
	height int4 NOT NULL,
	coinbase bool NOT NULL,
	pkscript bytea NOT NULL,
	CONSTRAINT utxos_pk PRIMARY KEY (txid, vout)
);
-- a wallet's outputs; a hash index takes scripts of any length
CREATE INDEX utxos_pkscript_idx ON public.utxos USING hash (pkscript);

-- The outputs the last blocks created, which a rollback deletes.
CREATE TABLE public.utxos_created (
	txid bytea NOT NULL,
	vout int4 NOT NULL,
	height int4 NOT NULL,
	CONSTRAINT utxos_created_pk PRIMARY KEY (txid, vout)
);
CREATE INDEX utxos_created_height_idx ON public.utxos_created USING btree (height);

-- The outputs the last blocks spent, which a rollback puts back.
-- `spent_height` is the block that spent the output.
CREATE TABLE public.utxos_spent (
	txid bytea NOT NULL,
	vout int4 NOT NULL,
	value int8 NOT NULL,
	height int4 NOT NULL,
	coinbase bool NOT NULL,
	pkscript bytea NOT NULL,
	spent_height int4 NOT NULL,
	CONSTRAINT utxos_spent_pk PRIMARY KEY (txid, vout)
);
CREATE INDEX utxos_spent_spent_height_idx ON public.utxos_spent USING btree (spent_height);

CREATE TABLE public.utxos_block_hashes (
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	CONSTRAINT utxos_block_hashes_pk PRIMARY KEY (block_height)
);

-- `undo_from` is the lowest height whose created and spent outputs
-- utxos_created and utxos_spent hold: a rollback reaches down to
-- undo_from - 1 and no further.
CREATE TABLE public.utxos_undo (
	id int2 NOT NULL DEFAULT 1,
	undo_from int4 NOT NULL,
	CONSTRAINT utxos_undo_pk PRIMARY KEY (id),
	CONSTRAINT utxos_undo_one_row CHECK (id = 1)
);
INSERT INTO public.utxos_undo (undo_from) VALUES (0);

CREATE TABLE public.utxos_network_type (
	id bigserial NOT NULL,
	network_type text NOT NULL,
	CONSTRAINT utxos_network_type_pk PRIMARY KEY (id)
);

CREATE TABLE public.utxos_indexer_version (
	id bigserial NOT NULL,
	indexer_version text NOT NULL,
	db_version int4 NOT NULL,
	CONSTRAINT utxos_indexer_version_pk PRIMARY KEY (id)
);
INSERT INTO public.utxos_indexer_version (indexer_version, db_version) VALUES ('opi-utxos v1.0.0', 1);
