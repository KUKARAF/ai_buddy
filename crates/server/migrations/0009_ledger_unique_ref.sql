-- Make wallet top-up idempotency atomic.
--
-- `credit_topup` treats a credit as already-processed when a
-- `(kind = 'topup', external_ref)` pair already exists in the ledger. Without a
-- unique constraint, that check-then-insert had a race: under concurrent
-- webhook redelivery two writers could both pass the existence check and both
-- credit the wallet (double-credit).
--
-- This partial unique index closes the race at the database layer: the second
-- concurrent insert fails with a unique-violation, which `credit_topup` converts
-- back into an idempotent no-op (rolling back its own balance bump). It is
-- scoped to `kind = 'topup' AND external_ref IS NOT NULL` so the other ledger
-- kinds (token_debit, pledge_*, payout) — which always carry a NULL
-- `external_ref` — are unaffected.
CREATE UNIQUE INDEX IF NOT EXISTS idx_ledger_topup_ref
    ON ledger_entries (kind, external_ref)
    WHERE kind = 'topup' AND external_ref IS NOT NULL;