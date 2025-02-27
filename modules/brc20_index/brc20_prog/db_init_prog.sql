CREATE TABLE public.brc20_prog_contracts (
	id bigserial NOT NULL,
	contract_address text NOT NULL,
	inscription_id text NOT NULL,
	block_height bigint NOT NULL,
	CONSTRAINT brc20_prog_contracts_pk PRIMARY KEY (id)
);
CREATE UNIQUE INDEX brc20_prog_contracts_inscription_id_idx ON public.brc20_prog_contracts USING btree (inscription_id);
CREATE INDEX brc20_prog_contracts_block_height_idx ON public.brc20_prog_contracts USING btree (block_height);