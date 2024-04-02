CREATE TABLE public.pow20_current_balances (
	id bigserial NOT NULL,
	pkscript text NOT NULL,
	wallet text NULL,
	tick varchar(4) NOT NULL,
	overall_balance numeric(40) NOT NULL,
	available_balance numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT pow20_current_balances_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX pow20_current_balances_pkscript_tick_idx ON public.pow20_current_balances USING btree (pkscript, tick);
CREATE INDEX pow20_current_balances_block_height_idx ON public.pow20_current_balances USING btree (block_height);
CREATE INDEX pow20_current_balances_pkscript_idx ON public.pow20_current_balances USING btree (pkscript);
CREATE INDEX pow20_current_balances_tick_idx ON public.pow20_current_balances USING btree (tick);
CREATE INDEX pow20_current_balances_wallet_idx ON public.pow20_current_balances USING btree (wallet);

CREATE TABLE public.pow20_unused_tx_inscrs (
	id bigserial NOT NULL,
	inscription_id text NOT NULL,
	tick varchar(4) NOT NULL,
	amount numeric(40) NOT NULL,
	current_holder_pkscript text NOT NULL,
	current_holder_wallet text NULL,
	event_id int8 NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT pow20_unused_tx_inscrs_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX pow20_unused_tx_inscrs_inscription_id_idx ON public.pow20_unused_tx_inscrs USING btree (inscription_id);
CREATE INDEX pow20_unused_tx_inscrs_tick_idx ON public.pow20_unused_tx_inscrs USING btree (tick);
CREATE INDEX pow20_unused_tx_inscrs_pkscript_idx ON public.pow20_unused_tx_inscrs USING btree (current_holder_pkscript);
CREATE INDEX pow20_unused_tx_inscrs_wallet_idx ON public.pow20_unused_tx_inscrs USING btree (current_holder_wallet);

CREATE TABLE public.pow20_extras_block_hashes (
	id bigserial NOT NULL,
	block_height int4 NOT NULL,
	block_hash text NOT NULL,
	CONSTRAINT pow20_extras_block_hashes_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX pow20_extras_block_hashes_block_height_idx ON public.pow20_extras_block_hashes USING btree (block_height);