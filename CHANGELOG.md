# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [Unreleased]

## [0.1.0] - 2026-10-05

Initial release: the accounting core, written against primary sources rather
than against existing code.

### Added

- `Ledger` — accounts, journal entries, period state, and an append-only audit
  trail. `post` refuses an entry that is unbalanced, names an unknown or
  non-postable account, mixes currencies, carries a sub-minor remainder, has a
  zero or single line, reuses an identifier, or lands in a closed period.
- `JournalEntry` with `Draft` → `Posted` → `Void` states. Posted entries are
  immutable: there is no API to edit or delete one.
- `reverse` — correction by counter-entry, every leg mirrored with its side
  flipped and its amount unchanged. Negating the amount as well would be a
  double negation that reverses nothing.
- `close_period` / `reopen_period`, where closing requires the period to balance
  and reopening requires a reason, because reopening a closed period is the one
  operation an auditor will ask about.
- An optimistic concurrency guard: a post carries the ledger version it was
  derived from and is refused if the ledger has moved on. Read-then-write is not
  a correctness argument — PostgreSQL's documentation notes that under
  `SERIALIZABLE` a transaction can still fail on a write it never saw.
- `Amount` — an exact integer at an explicit scale, no float. `RoundingMode` is a
  required argument with no `Default`, because IEEE 754 defaults to half-even
  while ZATCA E-Invoicing vF §10 mandates half-up. `quantize` refuses a second
  rounding of an already-rounded value (ISO 2 §3.3, GB/T 8170-2008 §3.3.1) and
  refuses to store a currency below its own minor unit.
- `Currency` with `minor_unit: Option<u32>`, so ISO 4217's `n.a.` (XAU, XDR,
  XUA) is distinguishable from a zero minor unit (JPY). MRO and MGA are refused:
  both are 1/5-scaled but coded as exponent 2, so an integer count of minor units
  cannot represent them.
