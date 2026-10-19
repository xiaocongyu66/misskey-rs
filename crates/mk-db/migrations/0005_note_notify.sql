-- note-notify arena (upstream test/e2e/note-notify.ts): the 'note'
-- notification reason. NoteCreateService.create L806 fans
-- `createNotification(following.followerId, 'note', { noteId }, user.id)`
-- out to every following row carrying notify = 'normal', and the pinned
-- MiNotification union (models/Notification.ts) leads with type: 'note'.
-- The 0001 baseline predates the reason (its enum stops at the 13-value
-- pre-'note' set) and the 0002 replay mirrors it, so the value lands here —
-- one ALTER per file (sqlx wraps each migration in its own txn; PG12+ only
-- bars USING the new value in the same transaction, never the ADD itself).
ALTER TYPE public.notification_type_enum ADD VALUE IF NOT EXISTS 'note' AFTER 'quote';
