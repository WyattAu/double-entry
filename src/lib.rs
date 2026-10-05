//! Append-only double-entry accounting: accounts, journal entries, posting,
//! period close, and reversal by counter-entry.
//!
//! # The design decision this crate starts from
//!
//! **No standard defines this.** ISO 20022's `StsId` is message-level, not
//! journal-entry-level; IFRS specifies recognition and derecognition and nothing
//! about immutability; nothing in ISO 4217 or the double-entry literature says
//! what `draft`/`posted`/`void` mean for a journal entry. So the model here is a
//! *documented choice* — modelled on TigerBeetle's append-only postings, the
//! closest implementable reference — and every rule below is stated as a rule,
//! not as a citation.
//!
//! # What is guaranteed
//!
//! - **Posted entries are immutable.** There is no API to edit or delete one.
//!   A correction is a new entry that reverses the old one, which is what makes
//!   the audit trail complete: every number in the ledger has a lineage.
//! - **An entry is balanced or it does not post.** Debits equal credits, in the
//!   entry's own currency, checked at post time and checked again by
//!   [`Ledger::assert_balanced`].
//! - **A closed period rejects postings.** And closing one requires the period
//!   to be balanced.
//! - **Posting is guarded against concurrent writers.** A post carries the
//!   expected prior version and is refused if the ledger has moved on. This is
//!   not paranoia about threads: PostgreSQL's own documentation notes that under
//!   SERIALIZABLE, check-then-insert can raise a unique violation that true
//!   serial execution never would, so a read followed by a write is not a
//!   correctness argument. The version is the correctness argument.

use core::fmt;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod money;

pub use money::{Amount, AmountError, Currency, RoundingMode, currencies};

/// A stable identifier for an account.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountId(pub String);

impl AccountId {
    /// Wrap an identifier string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The five classes an account can belong to.
///
/// The class decides the *normal balance*: assets and expenses increase on the
/// debit side, the rest on the credit side. This is what lets a trial balance be
/// checked without any per-account configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccountType {
    /// Assets. Normal balance debit.
    Asset,
    /// Liabilities. Normal balance credit.
    Liability,
    /// Owner's equity. Normal balance credit.
    Equity,
    /// Revenue. Normal balance credit.
    Revenue,
    /// Expenses. Normal balance debit.
    Expense,
}

impl AccountType {
    /// Whether an increase in this account type is a debit.
    pub fn increases_on_debit(self) -> bool {
        matches!(self, Self::Asset | Self::Expense)
    }

    /// The side this type normally sits on, as a signed balance for a positive
    /// amount.
    pub fn normal_balance_sign(self) -> i8 {
        if self.increases_on_debit() { 1 } else { -1 }
    }
}

/// An account in the chart of accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Its identifier.
    pub id: AccountId,
    /// Its name, for humans.
    pub name: String,
    /// Which of the five classes it belongs to.
    pub account_type: AccountType,
    /// Whether postings may name it. An income-summary or suspense account is
    /// often not postable by hand.
    pub postable: bool,
}

impl Account {
    /// A postable account.
    pub fn new(id: impl Into<String>, name: impl Into<String>, account_type: AccountType) -> Self {
        Self {
            id: AccountId::new(id),
            name: name.into(),
            account_type,
            postable: true,
        }
    }

    /// Mark the account as not postable by hand.
    pub fn non_postable(mut self) -> Self {
        self.postable = false;
        self
    }
}

/// Which side of an entry a line sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    /// The left-hand side.
    Debit,
    /// The right-hand side.
    Credit,
}

impl Side {
    /// The other side.
    pub fn opposite(self) -> Self {
        match self {
            Self::Debit => Self::Credit,
            Self::Credit => Self::Debit,
        }
    }

    /// The sign this side contributes to the entry's balance.
    pub fn sign(self) -> i8 {
        match self {
            Self::Debit => 1,
            Self::Credit => -1,
        }
    }
}

/// One leg of a journal entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The account debited or credited.
    pub account: AccountId,
    /// Which side.
    pub side: Side,
    /// The amount. **Unrounded**: rounding happens once, at the boundary the
    /// caller names (see [`money`]).
    pub amount: Amount,
    /// An optional note for this leg.
    pub memo: Option<String>,
}

impl Line {
    /// A debit leg.
    pub fn debit(account: impl Into<String>, amount: Amount) -> Self {
        Self {
            account: AccountId::new(account),
            side: Side::Debit,
            amount,
            memo: None,
        }
    }

    /// A credit leg.
    pub fn credit(account: impl Into<String>, amount: Amount) -> Self {
        Self {
            account: AccountId::new(account),
            side: Side::Credit,
            amount,
            memo: None,
        }
    }

    /// Attach a memo to this leg.
    pub fn with_memo(mut self, memo: impl Into<String>) -> Self {
        self.memo = Some(memo.into());
        self
    }

    /// This leg's amount in whole minor units, or `None` if it carries a
    /// sub-minor remainder.
    ///
    /// A posting must be a count of minor units: the rounding decision belongs
    /// at the boundary that produced the number (the invoice line, the tax
    /// computation), not here. A caller with a remainder should round it once,
    /// explicitly, with [`crate::money::Amount::quantize`].
    pub fn minor_units(&self) -> Option<i64> {
        self.amount.minor_units()
    }

    /// The signed contribution of this leg to the entry's balance: debits
    /// positive, credits negative.
    ///
    /// In **whole minor units**, not scaled units — a balance of 10.00 USD is
    /// 1000, so that every figure a report prints is a count of something.
    ///
    /// A sub-minor line contributes 0. `Ledger::post` refuses such lines, so a
    /// posted entry cannot contain one, and a draft is never summed; the case
    /// exists only so this accessor cannot panic.
    pub fn signed_minor(&self) -> i64 {
        self.minor_units().unwrap_or(0) * i64::from(self.side.sign())
    }
}

/// An entry's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryState {
    /// Editable. Not in the ledger: contributes nothing to any balance.
    Draft,
    /// Immutable and counted.
    Posted,
    /// An entry that has been reversed. Retained for lineage; contributes
    /// nothing, because its counter-entry does the work.
    Void,
}

impl EntryState {
    /// Whether an entry in this state affects balances.
    pub fn is_effective(self) -> bool {
        matches!(self, Self::Posted)
    }
}

/// A journal entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// Its identifier, unique within the ledger.
    pub id: String,
    /// The date the entry belongs to, as `YYYY-MM-DD`. A string on purpose:
    /// the ledger does not pick a calendar.
    pub date: String,
    /// The accounting period, as `YYYY-MM`. Posting is refused if it is closed.
    pub period: String,
    /// A description for the whole entry.
    pub memo: Option<String>,
    /// The legs.
    pub lines: Vec<Line>,
    /// Where it is in its lifecycle.
    pub state: EntryState,
    /// For a reversal, the entry it reverses.
    pub reverses: Option<String>,
    /// The version at last write, for the optimistic concurrency guard.
    pub version: u64,
}

impl JournalEntry {
    /// A draft entry.
    pub fn draft(
        id: impl Into<String>,
        date: impl Into<String>,
        period: impl Into<String>,
        lines: Vec<Line>,
    ) -> Self {
        Self {
            id: id.into(),
            date: date.into(),
            period: period.into(),
            memo: None,
            lines,
            state: EntryState::Draft,
            reverses: None,
            version: 0,
        }
    }

    /// Attach a memo to the entry.
    pub fn with_memo(mut self, memo: impl Into<String>) -> Self {
        self.memo = Some(memo.into());
        self
    }

    /// Total of the debit legs, in whole minor units.
    ///
    /// `None` when any debit leg carries a sub-minor remainder, which
    /// [`Ledger::post`] refuses. No currency argument: each [`Amount`] carries
    /// its own, and the ledger is what refuses an entry whose legs disagree. A
    /// parameter here would imply a check that does not happen at this level.
    pub fn total_debits(&self) -> Option<i64> {
        self.lines
            .iter()
            .filter(|l| l.side == Side::Debit)
            .map(Line::minor_units)
            .sum()
    }

    /// Total of the credit legs, in whole minor units.
    pub fn total_credits(&self) -> Option<i64> {
        self.lines
            .iter()
            .filter(|l| l.side == Side::Credit)
            .map(Line::minor_units)
            .sum()
    }

    /// Debits minus credits. Zero for a balanced entry.
    pub fn imbalance(&self) -> Option<i64> {
        Some(self.total_debits()? - self.total_credits()?)
    }

    /// Whether debits equal credits and every leg is a whole minor unit.
    pub fn is_balanced(&self) -> bool {
        self.imbalance() == Some(0)
    }
}

/// Why an entry could not be posted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostError {
    /// Fewer than two legs, so there is nothing to balance against.
    TooFewLines {
        /// How many legs were supplied.
        count: usize,
    },
    /// Debits do not equal credits.
    Unbalanced {
        /// Total debits, in minor units.
        debits: i64,
        /// Total credits, in minor units.
        credits: i64,
        /// The currency's ISO 4217 code.
        currency: &'static str,
    },
    /// A leg names an account the ledger does not have.
    UnknownAccount {
        /// The account that was named.
        account: String,
    },
    /// A leg names an account that may not be posted by hand.
    NotPostable {
        /// The account that was named.
        account: String,
    },
    /// Two legs in different currencies. A single entry cannot balance across
    /// currencies without an exchange rate, and inventing one silently is how
    /// FX gains appear from nowhere.
    MixedCurrencies {
        /// The currency of the first leg.
        expected: &'static str,
        /// The currency of the offending leg.
        found: &'static str,
    },
    /// The currency is one whose minor unit is not an integer count.
    UnsupportedCurrency {
        /// The currency's ISO 4217 code.
        currency: &'static str,
    },
    /// A leg carries a sub-minor remainder, so it is not a count of minor
    /// units. The rounding belongs at the boundary that produced the number.
    SubMinorAmount {
        /// The account that was named.
        account: String,
        /// The currency's ISO 4217 code.
        currency: &'static str,
    },
    /// A leg has a zero amount, which is noise.
    ZeroAmountLine {
        /// The account that was named.
        account: String,
    },
    /// The period is closed.
    PeriodClosed {
        /// The period, as `YYYY-MM`.
        period: String,
    },
    /// The entry identifier is already used.
    DuplicateId {
        /// The identifier.
        id: String,
    },
    /// The entry is not a draft, so it has nothing to post.
    NotADraft {
        /// The state it was in.
        state: EntryState,
    },
    /// Another writer moved the ledger on since this caller read it. The
    /// read-then-write that produced this expectation is not atomic, so it is
    /// refused rather than applied to a ledger it no longer describes.
    VersionConflict {
        /// The entry identifier.
        id: String,
        /// The version the caller expected.
        expected: u64,
        /// The version the ledger holds.
        actual: u64,
    },
    /// The entry to reverse is not posted, or does not exist.
    NotReversible {
        /// The entry that was named.
        id: String,
    },
    /// An entry already reverses that entry.
    AlreadyReversed {
        /// The entry that was named.
        id: String,
    },
}

impl fmt::Display for PostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewLines { count } => {
                write!(f, "an entry needs at least two lines, got {count}")
            }
            Self::Unbalanced {
                debits,
                credits,
                currency,
            } => write!(
                f,
                "debits {debits} do not equal credits {credits} (minor units of {currency})"
            ),
            Self::UnknownAccount { account } => write!(f, "no such account: {account}"),
            Self::NotPostable { account } => {
                write!(f, "account {account} is not postable by hand")
            }
            Self::MixedCurrencies { expected, found } => {
                write!(f, "entry mixes currencies: {expected} and {found}")
            }
            Self::UnsupportedCurrency { currency } => write!(
                f,
                "{currency}'s minor unit is not an integer count (ISO 4217 records \\
                 exponent 2 for a 1/5-scaled currency), so it cannot be represented"
            ),
            Self::SubMinorAmount { account, currency } => write!(
                f,
                "line on {account} is not a whole number of {currency} minor units; \
                 round once at the boundary that produced it, with an explicit \
                 RoundingMode"
            ),
            Self::ZeroAmountLine { account } => {
                write!(f, "line on {account} has a zero amount")
            }
            Self::PeriodClosed { period } => write!(f, "period {period} is closed"),
            Self::DuplicateId { id } => write!(f, "entry id {id} is already used"),
            Self::NotADraft { state } => write!(f, "entry is {state:?}, not a draft"),
            Self::VersionConflict {
                id,
                expected,
                actual,
            } => write!(
                f,
                "entry {id} was read at version {expected} but the ledger is at {actual}; \\
                 re-read and re-derive the entry"
            ),
            Self::NotReversible { id } => {
                write!(f, "entry {id} is not posted, so it cannot be reversed")
            }
            Self::AlreadyReversed { id } => write!(f, "entry {id} is already reversed"),
        }
    }
}

impl std::error::Error for PostError {}

/// Something that happened to the ledger, appended and never rewritten.
///
/// This is the audit trail. There is no method to remove or edit an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerEvent {
    /// A period was closed.
    PeriodClosed {
        /// The period, as `YYYY-MM`.
        period: String,
    },
    /// A period was reopened. Requires a reason, because reopening a closed
    /// period is the one operation an auditor will ask about.
    PeriodReopened {
        /// The period, as `YYYY-MM`.
        period: String,
        /// Why it was reopened.
        reason: String,
    },
    /// An entry was posted.
    Posted {
        /// The entry identifier.
        id: String,
        /// The entry's period.
        period: String,
        /// The expected version the caller held.
        expected_version: u64,
    },
    /// An entry was reversed by a counter-entry.
    Reversed {
        /// The original entry's identifier.
        id: String,
        /// The counter-entry's identifier.
        by: String,
    },
    /// An entry was voided.
    Voided {
        /// The entry identifier.
        id: String,
    },
}

/// The chart of accounts plus the journal, with period state and an audit trail.
#[derive(Debug, Default)]
pub struct Ledger {
    accounts: BTreeMap<AccountId, Account>,
    entries: BTreeMap<String, JournalEntry>,
    closed_periods: BTreeMap<String, String>,
    reversed: BTreeMap<String, String>,
    events: Vec<LedgerEvent>,
    version: AtomicU64,
}

impl Ledger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// The ledger's current version. A post carries the version it expected, so
    /// a concurrent writer is caught rather than overwritten.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Add an account. An existing identifier is replaced, because a chart of
    /// accounts is configuration rather than history.
    pub fn add_account(&mut self, account: Account) {
        self.accounts.insert(account.id.clone(), account);
    }

    /// Whether an account exists.
    pub fn has_account(&self, id: &AccountId) -> bool {
        self.accounts.contains_key(id)
    }

    /// An account by identifier.
    pub fn account(&self, id: &AccountId) -> Option<&Account> {
        self.accounts.get(id)
    }

    /// Whether a period is closed.
    pub fn is_period_closed(&self, period: &str) -> bool {
        self.closed_periods.contains_key(period)
    }

    /// Close a period.
    ///
    /// Refused while any posted entry in the period is unbalanced, and refused
    /// if it is already closed. Closing is what makes a period final: after it,
    /// no posting lands in it.
    pub fn close_period(&mut self, period: &str) -> Result<(), PostError> {
        if self.closed_periods.contains_key(period) {
            return Err(PostError::PeriodClosed {
                period: period.to_string(),
            });
        }
        self.assert_period_balanced(period)?;
        self.closed_periods
            .insert(period.to_string(), period.to_string());
        self.events.push(LedgerEvent::PeriodClosed {
            period: period.to_string(),
        });
        self.version.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Reopen a closed period. The reason is mandatory and recorded.
    pub fn reopen_period(&mut self, period: &str, reason: &str) -> Result<(), PostError> {
        if !self.closed_periods.contains_key(period) {
            return Err(PostError::PeriodClosed {
                period: period.to_string(),
            });
        }
        self.closed_periods.remove(period);
        self.events.push(LedgerEvent::PeriodReopened {
            period: period.to_string(),
            reason: reason.to_string(),
        });
        self.version.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Post an entry.
    ///
    /// `expected_version` is the ledger version the caller derived its entry
    /// from. If the ledger has moved on the post is refused: read-then-write is
    /// not a correctness argument, since a serializable transaction can still
    /// fail on a write the caller never saw.
    pub fn post(&mut self, entry: &JournalEntry, expected_version: u64) -> Result<u64, PostError> {
        let actual = self.version();
        if actual != expected_version {
            return Err(PostError::VersionConflict {
                id: entry.id.clone(),
                expected: expected_version,
                actual,
            });
        }
        if entry.state != EntryState::Draft {
            return Err(PostError::NotADraft { state: entry.state });
        }
        if self.entries.contains_key(&entry.id) {
            return Err(PostError::DuplicateId {
                id: entry.id.clone(),
            });
        }
        if self.is_period_closed(&entry.period) {
            return Err(PostError::PeriodClosed {
                period: entry.period.clone(),
            });
        }
        if entry.lines.len() < 2 {
            return Err(PostError::TooFewLines {
                count: entry.lines.len(),
            });
        }

        self.entry_currency(entry)?;
        for line in &entry.lines {
            if line.minor_units().is_none() {
                return Err(PostError::SubMinorAmount {
                    account: line.account.to_string(),
                    currency: line.amount.currency.alpha,
                });
            }
            if line.amount.units == 0 {
                return Err(PostError::ZeroAmountLine {
                    account: line.account.to_string(),
                });
            }
            match self.accounts.get(&line.account) {
                None => {
                    return Err(PostError::UnknownAccount {
                        account: line.account.to_string(),
                    });
                }
                Some(account) if !account.postable => {
                    return Err(PostError::NotPostable {
                        account: line.account.to_string(),
                    });
                }
                Some(_) => {}
            }
        }

        // Named for the offending leg, which the per-line loop above already
        // checked — so reaching here means a leg slipped through, and the
        // account is best-effort context for the error.
        let first_account = entry
            .lines
            .first()
            .map(|l| l.account.to_string())
            .unwrap_or_else(|| "<empty>".to_string());
        let currency = self.entry_currency(entry)?.alpha;
        let debits = entry.total_debits().ok_or(PostError::SubMinorAmount {
            account: first_account.clone(),
            currency,
        })?;
        let credits = entry.total_credits().ok_or(PostError::SubMinorAmount {
            account: first_account,
            currency,
        })?;
        if debits != credits {
            return Err(PostError::Unbalanced {
                debits,
                credits,
                currency: self.entry_currency(entry)?.alpha,
            });
        }

        let mut posted = entry.clone();
        posted.state = EntryState::Posted;
        posted.version = expected_version + 1;
        self.entries.insert(posted.id.clone(), posted);
        self.events.push(LedgerEvent::Posted {
            id: entry.id.clone(),
            period: entry.period.clone(),
            expected_version,
        });
        let next = self.version.fetch_add(1, Ordering::AcqRel) + 1;
        Ok(next)
    }

    /// Void an entry: a draft that will never post, or a posted entry that a
    /// correction supersedes. Voiding is recorded; the entry is retained.
    pub fn void(&mut self, id: &str) -> Result<(), PostError> {
        let entry = self
            .entries
            .get_mut(id)
            .ok_or_else(|| PostError::NotReversible { id: id.to_string() })?;
        entry.state = EntryState::Void;
        self.events.push(LedgerEvent::Voided { id: id.to_string() });
        self.version.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Reverse a posted entry by posting a counter-entry.
    ///
    /// Every leg is mirrored with its side flipped and its amount negated, so
    /// the counter-entry balances by construction. Returns the counter-entry's
    /// identifier.
    ///
    /// This is the only correction mechanism: nothing is ever edited or deleted,
    /// so every figure in a report can be traced to the entries that produced
    /// it and to the entry that undid it.
    pub fn reverse(
        &mut self,
        id: &str,
        counter_id: &str,
        date: &str,
        period: &str,
        memo: Option<String>,
        expected_version: u64,
    ) -> Result<String, PostError> {
        let original = self
            .entries
            .get(id)
            .ok_or_else(|| PostError::NotReversible { id: id.to_string() })?
            .clone();
        if original.state != EntryState::Posted {
            return Err(PostError::NotReversible { id: id.to_string() });
        }
        if self.reversed.contains_key(id) {
            return Err(PostError::AlreadyReversed { id: id.to_string() });
        }
        if original.period != period {
            return Err(PostError::PeriodClosed {
                period: original.period,
            });
        }

        let counter = JournalEntry {
            id: counter_id.to_string(),
            date: date.to_string(),
            period: period.to_string(),
            memo,
            // Every leg is mirrored with its side flipped and its amount
            // unchanged. Negating the amount *as well* would be a double
            // negation that reverses nothing at all: a credit of -12,000 is a
            // debit of 12,000.
            lines: original
                .lines
                .iter()
                .map(|l| Line {
                    account: l.account.clone(),
                    side: l.side.opposite(),
                    amount: l.amount.magnitude(),
                    memo: l.memo.clone(),
                })
                .collect(),
            state: EntryState::Draft,
            reverses: Some(id.to_string()),
            version: 0,
        };

        self.post(&counter, expected_version)?;
        self.reversed.insert(id.to_string(), counter_id.to_string());
        self.events.push(LedgerEvent::Reversed {
            id: id.to_string(),
            by: counter_id.to_string(),
        });
        Ok(counter_id.to_string())
    }

    /// An entry by identifier.
    pub fn entry(&self, id: &str) -> Option<&JournalEntry> {
        self.entries.get(id)
    }

    /// Every posted entry, in identifier order.
    pub fn posted_entries(&self) -> Vec<&JournalEntry> {
        self.entries
            .values()
            .filter(|e| e.state == EntryState::Posted)
            .collect()
    }

    /// The counter-entry that reversed `id`, if any.
    pub fn reversal_of(&self, id: &str) -> Option<&str> {
        self.reversed.get(id).map(String::as_str)
    }

    /// The ledger's audit trail, in order.
    pub fn events(&self) -> &[LedgerEvent] {
        &self.events
    }

    /// The balance of one account, in minor units, over posted entries.
    pub fn balance(&self, account: &AccountId) -> i64 {
        self.entries
            .values()
            .filter(|e| e.state == EntryState::Posted)
            .flat_map(|e| e.lines.iter())
            .filter(|l| &l.account == account)
            .map(Line::signed_minor)
            .sum()
    }

    /// Balances for every account with a non-zero balance, in identifier order.
    pub fn trial_balance(&self) -> BTreeMap<AccountId, i64> {
        let mut out: BTreeMap<AccountId, i64> = BTreeMap::new();
        for id in self.accounts.keys() {
            let balance = self.balance(id);
            if balance != 0 {
                out.insert(id.clone(), balance);
            }
        }
        out
    }

    /// Total debits and credits across posted entries, in minor units.
    pub fn totals(&self) -> (i64, i64) {
        let mut debits = 0;
        let mut credits = 0;
        for entry in self
            .entries
            .values()
            .filter(|e| e.state == EntryState::Posted)
        {
            for line in &entry.lines {
                let Some(minor) = line.minor_units() else {
                    continue;
                };
                match line.side {
                    Side::Debit => debits += minor,
                    Side::Credit => credits += minor,
                }
            }
        }
        (debits, credits)
    }

    /// Whether every posted entry balances.
    pub fn assert_balanced(&self) -> Result<(), PostError> {
        for entry in self
            .entries
            .values()
            .filter(|e| e.state == EntryState::Posted)
        {
            let currency = self.entry_currency(entry)?.alpha;
            let (Some(debits), Some(credits)) = (entry.total_debits(), entry.total_credits())
            else {
                continue;
            };
            if debits != credits {
                return Err(PostError::Unbalanced {
                    debits,
                    credits,
                    currency,
                });
            }
        }
        Ok(())
    }

    fn assert_period_balanced(&self, period: &str) -> Result<(), PostError> {
        for entry in self
            .entries
            .values()
            .filter(|e| e.state == EntryState::Posted && e.period == period)
        {
            let currency = self.entry_currency(entry)?.alpha;
            let (Some(debits), Some(credits)) = (entry.total_debits(), entry.total_credits())
            else {
                continue;
            };
            if debits != credits {
                return Err(PostError::Unbalanced {
                    debits,
                    credits,
                    currency,
                });
            }
        }
        Ok(())
    }

    /// The entry's currency, refusing mixed-currency and unrepresentable ones.
    fn entry_currency(&self, entry: &JournalEntry) -> Result<Currency, PostError> {
        let first = entry
            .lines
            .first()
            .ok_or(PostError::TooFewLines { count: 0 })?;
        let currency = first.amount.currency;
        if !currency.is_integral_minor_unit() {
            return Err(PostError::UnsupportedCurrency {
                currency: currency.alpha,
            });
        }
        for line in &entry.lines {
            let other = line.amount.currency;
            if other.alpha != currency.alpha {
                return Err(PostError::MixedCurrencies {
                    expected: currency.alpha,
                    found: other.alpha,
                });
            }
            if !other.is_integral_minor_unit() {
                return Err(PostError::UnsupportedCurrency {
                    currency: other.alpha,
                });
            }
        }
        Ok(currency)
    }
}
