//! Shared domain enums, stored in SQLite as lowercase TEXT and exchanged with
//! the frontend as JSON strings. Kept dependency-free so both server and mobile
//! can use them.

use serde::{Deserialize, Serialize};

macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }
        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some(Self::$variant),)+ _ => None }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum!(
    /// Lifecycle of a user's goal.
    GoalStatus {
        Draft => "draft",
        Active => "active",
        Succeeded => "succeeded",
        Failed => "failed",
        Abandoned => "abandoned",
    }
);

str_enum!(
    /// State of a single roadmap step.
    StepStatus {
        Pending => "pending",
        Done => "done",
        Skipped => "skipped",
    }
);

str_enum!(
    /// State of a money pledge attached to a goal.
    PledgeStatus {
        Proposed => "proposed",
        Held => "held",
        Forfeited => "forfeited",
        Refunded => "refunded",
    }
);

str_enum!(
    /// Kind of wallet ledger entry. Amounts are signed integer cents.
    LedgerKind {
        Topup => "topup",
        TokenDebit => "token_debit",
        PledgeHold => "pledge_hold",
        PledgeForfeit => "pledge_forfeit",
        PledgeRefund => "pledge_refund",
        Payout => "payout",
    }
);

str_enum!(
    /// Kind of scheduled notification.
    ReminderKind {
        Reminder => "reminder",
        Tip => "tip",
        Checkin => "checkin",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_str() {
        assert_eq!(GoalStatus::parse("active"), Some(GoalStatus::Active));
        assert_eq!(GoalStatus::Active.as_str(), "active");
        assert_eq!(
            LedgerKind::parse("token_debit"),
            Some(LedgerKind::TokenDebit)
        );
        assert_eq!(PledgeStatus::parse("nope"), None);
    }

    #[test]
    fn serde_uses_snake_case() {
        let j = serde_json::to_string(&LedgerKind::PledgeHold).unwrap();
        assert_eq!(j, "\"pledge_hold\"");
    }
}
