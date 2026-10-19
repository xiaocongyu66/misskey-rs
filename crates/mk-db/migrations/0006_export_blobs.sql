--
-- arena-exports: internal-storage byte lane for the export drive files
-- (upstream ExportClipsProcessorService → DriveService.addFile: the JSON
-- export lands as a drive file whose bytes the /files/{key}/{name} route
-- serves back). drive_file only carries metadata in this rewrite (the upload
-- path stores bytes nowhere, TODO(M4)); the export processor needs the bytes
-- fetchable, so they persist here keyed by the drive_file "accessKey" the
-- pack's url embeds — same accessor shape mk-queue used for mk_queue_jobs
-- (private table, FK-free, no baseline-schema dependencies). /api/_test/reset
-- truncates every public table, so harness epochs clear blobs + queue rows.
--

--
-- Name: mk_export_blobs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mk_export_blobs (
    access_key character varying(256) NOT NULL,
    content bytea NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

ALTER TABLE ONLY public.mk_export_blobs
    ADD CONSTRAINT mk_export_blobs_pkey PRIMARY KEY (access_key);
