--
-- mk-queue: durable job queue (mk-queue crate).
-- This rewrite replaces upstream's Redis/BullMQ broker (packages/backend/src/queue/*)
-- with a Postgres-backed queue; jobs are claimed FOR UPDATE SKIP LOCKED by
-- in-process workers (mk_queue::spawn_worker). FK-free, no dependencies on the
-- baseline schema.
--

--
-- Name: mk_queue_jobs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mk_queue_jobs (
    id bigserial NOT NULL,
    kind text NOT NULL,
    payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    run_after timestamp with time zone DEFAULT now() NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

--
-- Name: mk_queue_jobs_kind_run_after_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mk_queue_jobs_kind_run_after_idx ON public.mk_queue_jobs USING btree (kind, run_after);
