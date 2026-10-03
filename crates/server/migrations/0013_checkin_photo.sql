-- A check-in may carry a single photo. The bytes live on disk (under the
-- SQLite DB's parent dir, in `checkin_photos/<check_in_id>`); this column holds
-- the image's MIME type, and is NULL when no photo is attached.
ALTER TABLE check_ins ADD COLUMN photo_mime TEXT;
