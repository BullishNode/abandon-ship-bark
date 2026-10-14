-- Pool allocation change remains recoverable without reusable membership.
CREATE TABLE public.vtxo_pool_retirement (
	vtxo_id TEXT PRIMARY KEY REFERENCES public.vtxo(vtxo_id),
	pool_input_id TEXT NOT NULL REFERENCES public.vtxo_pool(vtxo_id)
);
