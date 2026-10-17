-- M2 notifications slice: the baseline 0001 has NO notification table — the
-- pinned upstream migration chain drops it in 1680582195041-cleanup.js
-- (notifications moved to Redis streams). This restores the FINAL pre-drop
-- table shape (union of 1000000000000-Init, 1585385921215-custom-notification,
-- 1674118260469-achievement) so notification rows are DB-truthful.

CREATE TABLE public.notification (
    "id" character varying(32) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "notifieeId" character varying(32) NOT NULL,
    "notifierId" character varying(32),
    "type" public.notification_type_enum NOT NULL,
    "isRead" boolean DEFAULT false NOT NULL,
    "noteId" character varying(32),
    "reaction" character varying(128),
    "choice" integer,
    "achievement" character varying(128),
    "customBody" character varying(2048),
    "customHeader" character varying(256),
    "customIcon" character varying(1024),
    "appAccessTokenId" character varying(32)
);

ALTER TABLE ONLY public.notification
    ADD CONSTRAINT "PK_705b6c7cdf9b2c2ff7ac7872cb7" PRIMARY KEY ("id");

CREATE INDEX "IDX_b11a5e627c41d4dc3170f1d370" ON public.notification ("createdAt");
CREATE INDEX "IDX_3c601b70a1066d2c8b517094cb" ON public.notification ("notifieeId");
CREATE INDEX "IDX_3b4e96eec8d36a8bbb9d02aa71" ON public.notification ("notifierId");
CREATE INDEX "IDX_33f33cc8ef29d805a97ff4628b" ON public.notification ("type");
CREATE INDEX "IDX_080ab397c379af09b9d2169e5b" ON public.notification ("isRead");

ALTER TABLE ONLY public.notification
    ADD CONSTRAINT "FK_3b4e96eec8d36a8bbb9d02aa710" FOREIGN KEY ("notifierId") REFERENCES public."user"("id") ON DELETE CASCADE ON UPDATE NO ACTION;
