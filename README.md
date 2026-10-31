# double-entry
[![Estate integration](https://github.com/WyattAu/estate-integration/actions/workflows/ci.yml/badge.svg)](https://github.com/WyattAu/estate-integration/actions/workflows/ci.yml)

Immutable double-entry accounting core: append-only journals, balanced posting,
period close, and correction by counter-entry.

**API-first. No UI.** This is the accounting core for an SME ledger, not an
application; it has no HTTP layer, no database, and no opinions about your
framework.

## Why this exists

No standard defines this. ISO 20022's `StsId` is message-level, not
journal-entry-level; IFRS specifies recognition and derecognition and says
nothing about immutability; nothing in ISO 4217 or the double-entry literature
defines `draft`/`posted`/`void` for a journal entry. So every rule here is a
**documented choice**, modelled on TigerBeetle's append-only postings; not a
citation. That is stated in the module docs too, so nobody later mistakes a
decision for a standard.

## The guarantees

| Guarantee | Mechanism |
|---|---|
| Posted entries are immutable | No API edits or deletes one; a correction is a counter-entry |
| An entry balances or it does not post | Debits equal credits, checked at post time |
| A closed period rejects postings | `close_period` also requires the period to balance |
| Reopening a period is auditable | `reopen_period` requires a reason and records it |
| Concurrent writers cannot clobber | A post carries the expected ledger version |
| Mixed currencies are refused | An entry cannot balance across currencies without inventing a rate |
| Sub-minor amounts are refused | The rounding belongs at the boundary that produced the number |

The version guard is worth spelling out: PostgreSQL's own documentation notes
that under `SERIALIZABLE` a transaction can fail on a unique violation that true
serial execution never would, so *check-then-write is not a correctness
argument*. A post carries the version it was derived from and is refused if the
ledger has moved on.

## Money, and why there is no default rounding mode

`Amount` is an exact integer at an explicit scale; no float anywhere. The
currency's exponent is data: 1000 JPY minor units is `1000`, and 1000 USD minor
units is `10.00`.

The rounding mode is a **required argument**, because the authorities disagree
and the hardware picks a side:

- IEEE 754's default is **half-even**, and so are ISO 80000-1 rule A, ASTM E29
  and OPC UA Part 8.
- ZATCA E-Invoicing vF §10 mandates **half-up**.
- "Swiss rounding" is not an ISO term, and UN/CEFACT Rec 20 is about units of
  measure, not rounding.

A money type that defaults to the platform's behaviour silently picks half-even,
so a ZATCA-filed invoice is off by a minor unit on every tie. Successive
rounding is *refused*, not merely discouraged: ISO 2 §3.3 and GB/T 8170-2008
§3.3.1 both forbid `97.46 → 97.5 → 98`.

Two currency modelling details that bite:

- `minor_unit` is an `Option`, because ISO 4217 records **`n.a.`** for XAU, XDR
  and XUA. That is not the same as a minor unit of zero (`JPY`), and collapsing
  them makes "is this integral?" unanswerable.
- **MRO and MGA are 1/5-scaled but ISO 4217 codes them as exponent `2`.** An
  integer count of minor units cannot represent a fifth, so they are refused
  rather than treated as decimal; which would be off by a factor of five on
  remittance with no error anywhere.

## Usage

```rust
use double_entry::{Account, AccountType, Amount, JournalEntry, Ledger, Line, currencies};

let mut ledger = Ledger::new();
ledger.add_account(Account::new("1200", "Accounts receivable", AccountType::Asset));
ledger.add_account(Account::new("4000", "Revenue", AccountType::Revenue));

let usd = Amount::minor(12_000, currencies::USD); // 120.00
let entry = JournalEntry::draft(
    "INV-1",
    "2026-03-01",
    "2026-03",
    vec![Line::debit("1200", usd), Line::credit("4000", usd)],
);

let version = ledger.version();
ledger.post(&entry, version)?;          // Ok(1)
ledger.close_period("2026-03")?;        // a balanced period can be closed

// The only correction mechanism. Nothing is edited; the numbers cancel.
let version = ledger.version();
ledger.reverse("INV-1", "REV-1", "2026-03-15", "2026-03", None, version)?;
# Ok::<(), double_entry::PostError>(())
```

## Not in v1

Payroll, inventory costing, FX revaluation, multi-currency entries (refused, not
silently converted). Those need decisions the sources do not settle yet.

## Licence

MIT OR Apache-2.0.
