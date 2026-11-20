# Pattern adoption

Relationship to the WyattAu pattern canon
(`github.com/WyattAu/pattern-standards`), validated by
`pattern-standards/scripts/adoption-check.py`.

| Pattern | Status | Evidence |
|---|---|---|
| T27 | adopted | src/lib.rs:669 (post refuses debits != credits), src/lib.rs:692 and src/lib.rs:712 (correction by counter-entry; no edit or delete API), tests/ledger.rs:231 |
| T21 | partial | src/lib.rs:470 (LedgerEvent log, appended and never rewritten) (audit trail only; ledger state is not rebuilt from events) |
| T33 | partial | src/lib.rs:608 (duplicate entry id refused at post) (in-process unique-key guard, not a delivery-level barrier) |
| T34 | partial | src/lib.rs:599 (stale expected_version refused), src/lib.rs:686 (monotonic version) (in-process only; no fencing token across a persistent store) |
| P55 | adopted | src/lib.rs:596 (a post carries expected_version and is refused if the ledger has moved on) |
| P11 | partial | src/money.rs:78 (SuccessiveRounding refused), src/money.rs:316 (rounded_to records the rounding scale) (single-rounding discipline only; no partition or allocation API) |
| P15 | partial | src/money.rs:297 (no floats anywhere; exact integer minor units) (the NaN failure class is removed by construction, not via a Finite wrapper) |
| P64 | planned | property-based conservation suite (proptest) plus a nightly PROPTEST_CASES soak workflow, target 2027-Q1 |
| P68 | adopted | tests/ledger.rs:26 (usd, ledger_with_accounts, post_simple builders; per-test fixtures, no shared mutable state) |
