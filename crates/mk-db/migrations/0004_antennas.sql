-- antennas-v2: note fanout join. The pinned upstream dropped the antenna_note
-- table when antennas/notes moved onto the redis fanout timeline
-- (FanoutTimelineService `antennaTimeline:{id}`); misskey-rs projects the same
-- feed into SQL so antennas/notes can filter/paginate deterministically.
-- Semantics mirror NoteCreateService.postNoteCreated →
-- AntennaService.addNoteToAntennas: a row is inserted ONLY at note-creation
-- time for each matching ACTIVE antenna (match is never recomputed at read
-- time), so notes posted before the antenna existed stay out and
-- antennas/remove-note deletes single ids out of the feed.

CREATE TABLE public.antenna_note (
    id character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL,
    "antennaId" character varying(32) NOT NULL,
    read boolean DEFAULT false NOT NULL
);

ALTER TABLE ONLY public.antenna_note
    ADD CONSTRAINT "PK_antenna_note" PRIMARY KEY (id);

CREATE INDEX "IDX_antenna_note_antennaId" ON public.antenna_note USING btree ("antennaId");

-- (antennaId, noteId) pair: addNoteToAntennas runs once per created note, the
-- unique index keeps the projection idempotent under handler retries.
CREATE UNIQUE INDEX "IDX_antenna_note_pair" ON public.antenna_note USING btree ("antennaId", "noteId");

ALTER TABLE ONLY public.antenna_note
    ADD CONSTRAINT "FK_antenna_note_note" FOREIGN KEY ("noteId") REFERENCES public."note"("id") ON DELETE CASCADE;

ALTER TABLE ONLY public.antenna_note
    ADD CONSTRAINT "FK_antenna_note_antenna" FOREIGN KEY ("antennaId") REFERENCES public."antenna"("id") ON DELETE CASCADE;
