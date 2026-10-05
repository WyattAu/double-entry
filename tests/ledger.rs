//! Behaviour tests for the double-entry core.
//!
//! Every case here is an invariant a ledger is asked to keep under load, not a
//! restatement of the API. The money cases come from named sources; the
//! ledger cases come from the failure modes that actually lose money.

// The library denies `expect_used` so a production path cannot panic. In a test,
// a failed setup assertion *is* the signal, so the same rules would only mean
// rewriting every assertion into a match.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use double_entry::{
    Account, AccountType, Amount, JournalEntry, Ledger, Line, PostError, RoundingMode, Side,
    currencies,
};

fn usd(minor_units: i64) -> Amount {
    Amount::minor(minor_units, currencies::USD)
}

fn ledger_with_accounts() -> Ledger {
    let mut ledger = Ledger::new();
    ledger.add_account(Account::new("1000", "Cash", AccountType::Asset));
    ledger.add_account(Account::new(
        "1200",
        "Accounts receivable",
        AccountType::Asset,
    ));
    ledger.add_account(Account::new(
        "2000",
        "Accounts payable",
        AccountType::Liability,
    ));
    ledger.add_account(Account::new("4000", "Revenue", AccountType::Revenue));
    ledger.add_account(Account::new(
        "5000",
        "Cost of goods sold",
        AccountType::Expense,
    ));
    ledger.add_account(Account::new(
        "3900",
        "Retained earnings",
        AccountType::Equity,
    ));
    ledger
}

fn post_simple(
    ledger: &mut Ledger,
    id: &str,
    period: &str,
    lines: Vec<Line>,
) -> Result<(), PostError> {
    let entry = JournalEntry::draft(id, "2026-03-01", period, lines);
    let version = ledger.version();
    ledger.post(&entry, version).map(|_| ())
}

// -- 1. the core invariant: debits equal credits, or it does not post -------

#[test]
fn a_balanced_entry_posts_and_moves_balances() {
    let mut ledger = ledger_with_accounts();

    // Invoice 120.00 USD: debit receivable, credit revenue.
    post_simple(
        &mut ledger,
        "INV-1",
        "2026-03",
        vec![
            Line::debit("1200", usd(12_000)).with_memo("Invoice 001"),
            Line::credit("4000", usd(12_000)),
        ],
    )
    .expect("a balanced entry posts");

    assert_eq!(
        ledger.balance(&double_entry::AccountId::new("1200")),
        12_000
    );
    assert_eq!(
        ledger.balance(&double_entry::AccountId::new("4000")),
        -12_000
    );
    assert!(
        ledger.entry("INV-1").expect("stored").state.is_effective(),
        "a posted entry counts towards balances"
    );

    // Revenue is credit-normal, so its signed balance is negative. That sign
    // convention is what makes the trial balance work without configuration.
    assert_eq!(
        AccountType::Revenue.normal_balance_sign(),
        -1,
        "revenue increases on the credit side"
    );
    assert_eq!(AccountType::Asset.normal_balance_sign(), 1);
    assert_eq!(Side::Debit.opposite(), Side::Credit);
}

#[test]
fn an_unbalanced_entry_is_refused_with_both_totals() {
    let mut ledger = ledger_with_accounts();
    let version = ledger.version();
    let entry = JournalEntry::draft(
        "BAD-1",
        "2026-03-01",
        "2026-03",
        vec![
            Line::debit("1200", usd(12_000)),
            Line::credit("4000", usd(11_999)),
        ],
    );

    match ledger.post(&entry, version) {
        Err(PostError::Unbalanced {
            debits,
            credits,
            currency,
        }) => {
            assert_eq!(debits, 12_000);
            assert_eq!(credits, 11_999);
            assert_eq!(currency, "USD");
        }
        other => panic!("expected Unbalanced, got {other:?}"),
    }

    // Nothing moved, and the failure is legible enough to act on.
    assert_eq!(ledger.balance(&double_entry::AccountId::new("1200")), 0);
    assert!(ledger.posted_entries().is_empty());
}

#[test]
fn structural_defects_are_refused_before_arithmetic() {
    let mut ledger = ledger_with_accounts();

    // One line: nothing to balance against.
    let version = ledger.version();
    let single = JournalEntry::draft(
        "ONE",
        "2026-03-01",
        "2026-03",
        vec![Line::debit("1200", usd(100))],
    );
    assert!(matches!(
        ledger.post(&single, version),
        Err(PostError::TooFewLines { count: 1 })
    ));

    // An account that does not exist.
    let version = ledger.version();
    let unknown = JournalEntry::draft(
        "UNKNOWN",
        "2026-03-01",
        "2026-03",
        vec![
            Line::debit("9999", usd(100)),
            Line::credit("4000", usd(100)),
        ],
    );
    assert!(matches!(
        ledger.post(&unknown, version),
        Err(PostError::UnknownAccount { .. })
    ));

    // A zero-amount leg is noise, not information.
    let version = ledger.version();
    let zero = JournalEntry::draft(
        "ZERO",
        "2026-03-01",
        "2026-03",
        vec![Line::debit("1200", usd(0)), Line::credit("4000", usd(0))],
    );
    assert!(matches!(
        ledger.post(&zero, version),
        Err(PostError::ZeroAmountLine { .. })
    ));

    // A non-postable account is refused by name.
    ledger
        .add_account(Account::new("3900", "Retained earnings", AccountType::Equity).non_postable());
    let version = ledger.version();
    let blocked = JournalEntry::draft(
        "BLOCKED",
        "2026-03-01",
        "2026-03",
        vec![
            Line::debit("3900", usd(100)),
            Line::credit("4000", usd(100)),
        ],
    );
    assert!(matches!(
        ledger.post(&blocked, version),
        Err(PostError::NotPostable { .. })
    ));
}

// -- 2. mixed currencies are refused, not converted -----------------------

#[test]
fn an_entry_cannot_mix_currencies() {
    let mut ledger = ledger_with_accounts();
    let version = ledger.version();
    let mixed = JournalEntry::draft(
        "MIXED",
        "2026-03-01",
        "2026-03",
        vec![
            Line::debit("1200", usd(12_000)),
            Line::credit("4000", Amount::minor(12_000, currencies::EUR)),
        ],
    );

    match ledger.post(&mixed, version) {
        Err(PostError::MixedCurrencies { expected, found }) => {
            assert_eq!(expected, "USD");
            assert_eq!(found, "EUR");
        }
        other => panic!("expected MixedCurrencies, got {other:?}"),
    }
}

// -- 3. immutability: corrections are counter-entries ----------------------

#[test]
fn a_posted_entry_is_corrected_by_a_counter_entry_never_by_editing() {
    let mut ledger = ledger_with_accounts();
    post_simple(
        &mut ledger,
        "INV-1",
        "2026-03",
        vec![
            Line::debit("1200", usd(12_000)),
            Line::credit("4000", usd(12_000)),
        ],
    )
    .expect("posts");
    assert_eq!(
        ledger.balance(&double_entry::AccountId::new("1200")),
        12_000
    );

    let version = ledger.version();
    let counter = ledger
        .reverse(
            "INV-1",
            "REV-1",
            "2026-03-15",
            "2026-03",
            Some("wrong amount".into()),
            version,
        )
        .expect("reversal posts");
    assert_eq!(counter, "REV-1");

    // The original is untouched and still readable — lineage is intact.
    let original = ledger.entry("INV-1").expect("original retained");
    assert_eq!(original.state, double_entry::EntryState::Posted);
    assert_eq!(original.total_debits(), Some(12_000));

    // The counter-entry mirrors every leg with its side flipped.
    let counter_entry = ledger.entry("REV-1").expect("counter-entry stored");
    assert_eq!(counter_entry.reverses.as_deref(), Some("INV-1"));
    assert_eq!(counter_entry.lines.len(), 2);
    // The amount is a magnitude and the side carries the direction, so the
    // counter-entry is a credit of 12_000 against a debit of 12_000. Negating
    // the amount *as well* as flipping the side would be a double negation that
    // reverses nothing.
    assert_eq!(counter_entry.lines[0].side, Side::Credit);
    assert_eq!(counter_entry.lines[0].amount.minor_units(), Some(12_000));
    assert_eq!(counter_entry.lines[0].amount.units, 12_000);
    assert_eq!(counter_entry.lines[1].side, Side::Debit);
    assert_eq!(counter_entry.lines[1].amount.minor_units(), Some(12_000));

    // Net effect: zero. The two entries are both in the ledger; the arithmetic
    // is what makes them cancel, not a deletion.
    assert_eq!(ledger.balance(&double_entry::AccountId::new("1200")), 0);
    assert_eq!(ledger.balance(&double_entry::AccountId::new("4000")), 0);
    assert_eq!(ledger.reversal_of("INV-1"), Some("REV-1"));
}

#[test]
fn an_entry_cannot_be_reversed_twice() {
    let mut ledger = ledger_with_accounts();
    post_simple(
        &mut ledger,
        "INV-1",
        "2026-03",
        vec![
            Line::debit("1200", usd(500)),
            Line::credit("4000", usd(500)),
        ],
    )
    .expect("posts");

    let version = ledger.version();
    ledger
        .reverse("INV-1", "REV-1", "2026-03-15", "2026-03", None, version)
        .expect("first reversal");

    let version = ledger.version();
    assert!(matches!(
        ledger.reverse("INV-1", "REV-2", "2026-03-16", "2026-03", None, version),
        Err(PostError::AlreadyReversed { .. })
    ));
}

// -- 4. period close -------------------------------------------------------

#[test]
fn a_closed_period_refuses_postings() {
    let mut ledger = ledger_with_accounts();
    post_simple(
        &mut ledger,
        "INV-1",
        "2026-02",
        vec![
            Line::debit("1200", usd(1_000)),
            Line::credit("4000", usd(1_000)),
        ],
    )
    .expect("posts");

    ledger
        .close_period("2026-02")
        .expect("a balanced period closes");

    let version = ledger.version();
    let late = JournalEntry::draft(
        "LATE",
        "2026-02-28",
        "2026-02",
        vec![
            Line::debit("1200", usd(1_000)),
            Line::credit("4000", usd(1_000)),
        ],
    );
    assert!(matches!(
        ledger.post(&late, version),
        Err(PostError::PeriodClosed { .. })
    ));

    // Closing twice is also refused.
    assert!(ledger.close_period("2026-02").is_err());

    // Reopening needs a reason and records it, because it is the operation an
    // auditor asks about.
    ledger
        .reopen_period("2026-02", "late-arriving supplier invoice")
        .expect("reopens");
    let version = ledger.version();
    ledger.post(&late, version).expect("posts once reopened");
    assert!(ledger.events().iter().any(
        |e| matches!(e, double_entry::LedgerEvent::PeriodReopened { reason, .. }
            if reason == "late-arriving supplier invoice")
    ));
}

// -- 5. concurrency: the version guard ------------------------------------

/// A read-then-write is not a correctness argument. PostgreSQL's own
/// documentation notes that under SERIALIZABLE a transaction can still fail on a
/// write it never saw, so a ledger that trusts "I read it a moment ago" will
/// eventually write to a ledger it no longer describes.
#[test]
fn a_stale_expected_version_is_refused() {
    let mut ledger = ledger_with_accounts();

    // Two callers both read the ledger here.
    let stale = ledger.version();
    assert!(
        post_simple(
            &mut ledger,
            "A",
            "2026-03",
            vec![
                Line::debit("1200", usd(1_000)),
                Line::credit("4000", usd(1_000))
            ]
        )
        .is_ok()
    );

    // The second caller still holds the old version.
    let entry = JournalEntry::draft(
        "B",
        "2026-03-01",
        "2026-03",
        vec![
            Line::debit("1200", usd(2_000)),
            Line::credit("4000", usd(2_000)),
        ],
    );
    match ledger.post(&entry, stale) {
        Err(PostError::VersionConflict {
            expected, actual, ..
        }) => {
            assert_eq!(expected, stale);
            assert!(
                actual > stale,
                "the ledger has moved on: {actual} > {stale}"
            );
        }
        other => panic!("expected VersionConflict, got {other:?}"),
    }

    // Re-reading and re-deriving succeeds.
    let fresh = ledger.version();
    ledger.post(&entry, fresh).expect("the retry succeeds");
}

// -- 6. money: rounding, once, explicitly ---------------------------------

/// ISO 2 §3.3 and GB/T 8170-2008 §3.3.1 forbid successive rounding, and
/// ZATCA E-Invoicing vF §10 mandates half-up where ISO 80000-1 says half-even.
/// So the mode is an argument and a second rounding is refused.
#[test]
fn rounding_is_explicit_and_happens_once() {
    // The tie: 0.005 USD.
    // units/scale: 5 at scale 3 is 0.005, the tie between 0.00 and 0.01.
    let tie = Amount::scaled(5, 3, currencies::USD);

    let half_up = tie.quantize(2, RoundingMode::HalfUp).expect("rounds");
    assert_eq!(half_up.units, 1, "half-up sends 0.005 to 0.01");
    assert_eq!(half_up.to_decimal_string(), "0.01");

    let half_even = tie.quantize(2, RoundingMode::HalfEven).expect("rounds");
    assert_eq!(
        half_even.units, 0,
        "half-even sends 0.005 to 0.00, the even neighbour"
    );

    let away = tie.quantize(2, RoundingMode::AwayFromZero).expect("rounds");
    assert_eq!(away.units, 1);

    let toward = tie.quantize(2, RoundingMode::TowardZero).expect("rounds");
    assert_eq!(toward.units, 0);

    // A tie away from zero must actually go away from it, not merely up.
    let negative_tie = Amount::scaled(-5, 3, currencies::USD);
    assert_eq!(
        negative_tie
            .quantize(2, RoundingMode::AwayFromZero)
            .expect("rounds")
            .units,
        -1,
        "half-up on -0.005 gives -0.01, not -0.00"
    );
    assert_eq!(
        negative_tie
            .quantize(2, RoundingMode::HalfUp)
            .expect("rounds")
            .units,
        -1
    );
    assert_eq!(
        negative_tie
            .quantize(2, RoundingMode::TowardZero)
            .expect("rounds")
            .units,
        0
    );
    assert_eq!(
        negative_tie
            .quantize(2, RoundingMode::HalfEven)
            .expect("rounds")
            .units,
        0,
        "half-even takes the even neighbour, 0.00"
    );

    // A currency cannot be rounded below its own minor unit. USD has two
    // decimal places, so 97.46 -> 97.5 is not a rounding decision this ledger
    // is entitled to make: the value would no longer be a count of cents.
    assert!(
        Amount::scaled(9_746, 2, currencies::USD)
            .quantize(1, RoundingMode::HalfUp)
            .is_err(),
        "USD cannot be stored at one decimal place"
    );

    // Successive rounding, on a currency where an intermediate scale is legal.
    // 97.46 -> 97.5 -> 98 is not 97.5: ISO 2 section 3.3 and GB/T 8170-2008
    // section 3.3.1 both forbid it, so the second rounding is refused rather
    // than applied. XBT has no minor unit, so scale 2 and scale 1 are both
    // representable and the successive-rounding rule is the only thing standing
    // between the caller and 98.
    let once = Amount::scaled(9_746, 2, currencies::XBT)
        .quantize(1, RoundingMode::HalfUp)
        .expect("first rounding");
    assert_eq!(
        once.to_decimal_string(),
        "97.5",
        "97.46 rounds to 97.5 at one decimal"
    );
    assert!(
        once.quantize(0, RoundingMode::HalfUp).is_err(),
        "and rounding 97.5 again is the successive rounding ISO 2 section 3.3 forbids"
    );
    // Re-rounding at the same scale is a no-op, not a second rounding.
    assert_eq!(
        once.quantize(1, RoundingMode::HalfUp)
            .expect("idempotent at the same scale")
            .to_decimal_string(),
        "97.5"
    );
}

#[test]
fn the_currency_minor_unit_is_data_not_an_assumption() {
    // JPY has no decimal places, so 1000 minor units is 1000 yen and not
    // 10.00. An implementation that assumes two decimals is wrong by 100x.
    let jpy = Amount::minor(1_000, currencies::JPY);
    assert_eq!(jpy.to_decimal_string(), "1000");

    let usd = usd(1_000);
    assert_eq!(usd.to_decimal_string(), "10.00");

    // ISO 4217 records `n.a.` for XBT — no minor unit at all — which is not the
    // same as a minor unit of zero.
    assert_eq!(currencies::XBT.scale(), None);
    assert_eq!(currencies::JPY.scale(), Some(0));
    assert_ne!(currencies::XBT.scale(), Some(0));

    // And a currency with no minor unit cannot be quantized to one.
    let btc = Amount::scaled(5, 3, currencies::XBT);
    assert!(btc.quantize_to_currency(RoundingMode::HalfUp).is_err());
    assert!(
        Amount::scaled(7, 0, currencies::XBT)
            .quantize_to_currency(RoundingMode::HalfUp)
            .is_err()
    );
}

/// ISO 4217 codes MRO and MGA as exponent 2, but both are 1/5-scaled. An
/// integer count of minor units cannot represent a fifth, so treating them as
/// decimal is off by a factor of five on remittance with no error anywhere.
#[test]
fn one_fifth_scaled_currencies_are_refused_rather_than_misrepresented() {
    let mro = double_entry::Currency {
        alpha: "MRO",
        numeric: "478",
        minor_unit: Some(2),
    };
    assert!(!mro.is_integral_minor_unit());
    assert!(currencies::JPY.is_integral_minor_unit());

    let mut ledger = ledger_with_accounts();
    let version = ledger.version();
    let entry = JournalEntry::draft(
        "MRO-1",
        "2026-03-01",
        "2026-03",
        vec![
            Line::debit("1200", Amount::minor(1_000, mro)),
            Line::credit("4000", Amount::minor(1_000, mro)),
        ],
    );
    match ledger.post(&entry, version) {
        Err(PostError::UnsupportedCurrency { currency }) => assert_eq!(currency, "MRO"),
        other => panic!("expected UnsupportedCurrency, got {other:?}"),
    }
}

// -- 7. the trial balance actually balances --------------------------------

#[test]
fn a_full_year_of_entries_balances_every_time() {
    let mut ledger = ledger_with_accounts();

    // Sale on credit.
    post_simple(
        &mut ledger,
        "INV-1",
        "2026-01",
        vec![
            Line::debit("1200", usd(500_000)),
            Line::credit("4000", usd(500_000)),
        ],
    )
    .expect("posts");
    // Customer pays.
    post_simple(
        &mut ledger,
        "PAY-1",
        "2026-01",
        vec![
            Line::debit("1000", usd(500_000)),
            Line::credit("1200", usd(500_000)),
        ],
    )
    .expect("posts");
    // Buy inventory on credit.
    post_simple(
        &mut ledger,
        "BILL-1",
        "2026-02",
        vec![
            Line::debit("5000", usd(200_000)),
            Line::credit("2000", usd(200_000)),
        ],
    )
    .expect("posts");
    // Pay the supplier.
    post_simple(
        &mut ledger,
        "PAY-2",
        "2026-02",
        vec![
            Line::debit("2000", usd(200_000)),
            Line::credit("1000", usd(200_000)),
        ],
    )
    .expect("posts");

    assert_eq!(
        ledger.balance(&double_entry::AccountId::new("1200")),
        0,
        "receivable cleared"
    );
    assert_eq!(
        ledger.balance(&double_entry::AccountId::new("2000")),
        0,
        "payable cleared"
    );
    assert_eq!(
        ledger.balance(&double_entry::AccountId::new("1000")),
        300_000,
        "cash remains"
    );
    assert_eq!(
        ledger.balance(&double_entry::AccountId::new("4000")),
        -500_000,
        "revenue"
    );
    assert_eq!(
        ledger.balance(&double_entry::AccountId::new("5000")),
        200_000,
        "cogs"
    );

    ledger
        .assert_balanced()
        .expect("every posted entry balances");

    let (debits, credits) = ledger.totals();
    assert_eq!(debits, credits, "the ledger's own totals agree");

    // And the trial balance agrees: debits minus credits across all accounts is
    // zero, which is the accounting equation.
    let trial = ledger.trial_balance();
    let net: i64 = trial.values().sum();
    assert_eq!(net, 0, "the trial balance nets to zero: {trial:?}");

    assert_eq!(trial.len(), 3, "only the accounts with movements appear");
    // The accounting equation, checked account by account against its class.
    for (id, balance) in &trial {
        let account = ledger
            .account(id)
            .expect("trial balance names known accounts");
        let balance = *balance;
        if balance > 0 {
            assert!(
                account.account_type.increases_on_debit(),
                "{id} has a positive balance so it must be debit-normal"
            );
        } else if balance < 0 {
            assert!(
                !account.account_type.increases_on_debit(),
                "{id} has a negative balance so it must be credit-normal"
            );
        }
    }
}

#[test]
fn a_voided_entry_stops_counting_but_is_retained() {
    let mut ledger = ledger_with_accounts();
    post_simple(
        &mut ledger,
        "DRAFT-ISH",
        "2026-03",
        vec![
            Line::debit("1200", usd(900)),
            Line::credit("4000", usd(900)),
        ],
    )
    .expect("posts");
    assert_eq!(ledger.balance(&double_entry::AccountId::new("1200")), 900);

    ledger.void("DRAFT-ISH").expect("voids");
    assert_eq!(ledger.balance(&double_entry::AccountId::new("1200")), 0);
    // Still readable: the entry is not deleted, it is marked.
    assert_eq!(
        ledger.entry("DRAFT-ISH").expect("retained").state,
        double_entry::EntryState::Void
    );
    assert!(ledger.posted_entries().is_empty());
    assert!(
        ledger
            .events()
            .iter()
            .any(|e| matches!(e, double_entry::LedgerEvent::Voided { id } if id == "DRAFT-ISH"))
    );
}

#[test]
fn a_duplicate_identifier_is_refused() {
    let mut ledger = ledger_with_accounts();
    post_simple(
        &mut ledger,
        "INV-1",
        "2026-03",
        vec![
            Line::debit("1200", usd(100)),
            Line::credit("4000", usd(100)),
        ],
    )
    .expect("posts");

    let version = ledger.version();
    let again = JournalEntry::draft(
        "INV-1",
        "2026-03-02",
        "2026-03",
        vec![
            Line::debit("1200", usd(200)),
            Line::credit("4000", usd(200)),
        ],
    );
    assert!(matches!(
        ledger.post(&again, version),
        Err(PostError::DuplicateId { .. })
    ));
}
