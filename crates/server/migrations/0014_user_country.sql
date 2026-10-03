-- Per-user COUNTRY for the holiday-aware planner.
--
-- ISO-3166-1 alpha-2 (e.g. "DE", "PL"), stored uppercased. NULL means the
-- country is unknown/unset. It is either chosen explicitly by the user or
-- client-side pre-filled from an OPTIONAL offline IP->country lookup; the
-- client IP used for that lookup is never persisted.

ALTER TABLE user_settings ADD COLUMN country TEXT;
