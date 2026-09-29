-- Wallet, ledger, and pledge schema.
--
-- Money is stored as INTEGER cents everywhere. Timestamps are TEXT (RFC 3339).
-- `wallets.balance_cents` is authoritative for spendable balance; the balance
-- covers both LLM token usage (`token_spend_cents` tracks the cumulative spend)
-- and funds pledged to goals. `ledger_entries` is an append-only audit log.

CREATE TABLE wallets (
    user_id           TEXT PRIMARY KEY,
    balance_cents     INTEGER NOT NULL DEFAULT 0,
    currency          TEXT NOT NULL DEFAULT 'EUR',
    token_spend_cents INTEGER NOT NULL DEFAULT 0,
    updated_at        TEXT NOT NULL
);

-- Append-only audit log of every balance-affecting event. `kind` is one of the
-- `LedgerKind` values (topup, token_debit, pledge_hold, pledge_forfeit,
-- pledge_refund, payout). `amount_cents` is signed (credits positive, debits
-- negative). `goal_id`/`external_ref`/`memo` are contextual and may be NULL.
CREATE TABLE ledger_entries (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL,
    kind         TEXT NOT NULL,
    amount_cents INTEGER NOT NULL,
    goal_id      TEXT,
    external_ref TEXT,
    memo         TEXT,
    created_at   TEXT NOT NULL
);

CREATE INDEX idx_ledger_entries_user_created ON ledger_entries(user_id, created_at);

-- One pledge per goal. `goal_id` is plain TEXT with no foreign key so this
-- migration does not depend on the goals table's creation order.
CREATE TABLE pledges (
    id           TEXT PRIMARY KEY,
    goal_id      TEXT NOT NULL UNIQUE,
    user_id      TEXT NOT NULL,
    amount_cents INTEGER NOT NULL,
    status       TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    resolved_at  TEXT
);
