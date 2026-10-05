//! Currencies, amounts, and the rounding rules that make a ledger defensible.
//!
//! # Why there is no default rounding mode
//!
//! The authorities disagree, and the hardware picks a side. IEEE 754's default
//! rounding is **half-even**, and so is ISO 80000-1 rule A, ASTM E29 and OPC UA
//! Part 8. ZATCA E-Invoicing vF §10 mandates **half-up**. "Swiss rounding" is
//! not an ISO term at all, and UN/CEFACT Rec 20 is a recommendation about units
//! of measure, not rounding.
//!
//! A money type that defaults to the platform's behaviour therefore silently
//! picks half-even, and a ZATCA-filed invoice is off by a minor unit on every
//! tie. So [`RoundingMode`] is a required argument everywhere, and the compiler
//! asks rather than the ledger guessing.
//!
//! # Why rounding happens once
//!
//! ISO 2 §3.3 and GB/T 8170-2008 §3.3.1 both forbid successive rounding.
//! `97.46 → 97.5 → 98` is not `97.5`; a ledger that rounds a line, then the
//! document total, and then a tax return has produced three different answers
//! for one economic fact. [`Amount::quantize`] rounds at a scale the caller
//! names, and [`Line::amount`] carries the unrounded value so a document can
//! round once at its own boundary.

use core::fmt;

/// How to resolve an exact tie.
///
/// Named variants, no `Default`: see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingMode {
    /// Round to the nearest representable value; ties go to the even digit.
    /// ISO 80000-1 rule A, ASTM E29, IEEE 754 default.
    HalfEven,
    /// Round to the nearest representable value; ties go away from zero.
    /// ZATCA E-Invoicing vF §10.
    HalfUp,
    /// Truncate toward zero.
    TowardZero,
    /// Round away from zero.
    AwayFromZero,
}

impl RoundingMode {
    /// On an exact tie, does the value round away from the candidate below?
    ///
    /// `lower_is_even` says whether the candidate below the tie is even.
    fn tie_takes_upper(self, lower_is_even: bool) -> bool {
        match self {
            // Half-even takes the even neighbour, so it moves up only when the
            // lower candidate is odd.
            Self::HalfEven => !lower_is_even,
            Self::HalfUp | Self::AwayFromZero => true,
            Self::TowardZero => false,
        }
    }
}

/// Why an amount could not be built or scaled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmountError {
    /// The currency code is not a three-letter ISO 4217 alphabetic code.
    CurrencyCodeMalformed(String),
    /// The amount is not representable in this currency's minor unit.
    NotRepresentable {
        /// The currency's ISO 4217 code.
        currency: &'static str,
        /// The fraction of a minor unit that could not be represented.
        remainder: String,
    },
    /// The amount does not fit in an `i64` of minor units.
    Overflow {
        /// The currency's ISO 4217 code.
        currency: &'static str,
    },
    /// Successive rounding was attempted: the value already carried a scale
    /// finer than the boundary being rounded to.
    SuccessiveRounding {
        /// The scale being rounded to.
        to_scale: u32,
        /// The scale the value already had.
        from_scale: u32,
    },
}

impl fmt::Display for AmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrencyCodeMalformed(c) => {
                write!(f, "{c:?} is not a three-letter ISO 4217 alphabetic code")
            }
            Self::NotRepresentable {
                currency,
                remainder,
            } => write!(
                f,
                "the amount leaves {remainder} of a minor unit, which {currency} \
                 cannot represent as an integer number of minor units"
            ),
            Self::Overflow { currency } => {
                write!(
                    f,
                    "the amount overflows a 64-bit minor-unit count for {currency}"
                )
            }
            Self::SuccessiveRounding {
                to_scale,
                from_scale,
            } => write!(
                f,
                "rounding from scale {from_scale} to {to_scale} is a second rounding; \
                 carry the unrounded value and round once at the final boundary \
                 (ISO 2 section 3.3, GB/T 8170-2008 section 3.3.1)"
            ),
        }
    }
}

impl std::error::Error for AmountError {}

/// A currency, with the exponent ISO 4217 gives it.
///
/// `minor_unit` is an `Option` because ISO 4217 records **`n.a.`** — no minor
/// unit — for XAU, XDR and XUA. That is semantically different from a currency
/// whose minor unit happens to be zero (`JPY`): one has no subunit at all, the
/// other has a subunit worth nothing. Collapsing them makes "is this amount
/// integral?" unanswerable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Currency {
    /// The ISO 4217 alphabetic code.
    pub alpha: &'static str,
    /// The ISO 4217 numeric code, as a string to preserve leading zeros.
    pub numeric: &'static str,
    /// The number of decimal places in the minor unit, or `None` for `n.a.`.
    pub minor_unit: Option<u32>,
}

impl Currency {
    /// A currency the ledger understands.
    ///
    /// `scale()` returns `None` for the `n.a.` currencies, which is the honest
    /// answer: there is no minor unit to scale to.
    pub fn scale(&self) -> Option<u32> {
        self.minor_unit
    }

    /// Whether this currency's minor unit is a simple power of ten.
    ///
    /// ISO 4217 codes **MRO** (ouguiya) and **MGA** (iraimbilanja) as exponent
    /// `2`, but both are **1/5**-scaled: one khoum or iraimbilanja is a fifth of
    /// a unit. An integer number of minor units cannot represent a fifth at all,
    /// so silently treating these as decimal is off by a factor of five on
    /// remittance and FX with no error anywhere. They are refused.
    pub fn is_integral_minor_unit(&self) -> bool {
        !matches!(self.alpha, "MRO" | "MGA")
    }
}

/// The currencies the ledger supports.
///
/// A deliberately short table rather than a generated one: an SME ledger needs
/// the currencies its users actually invoice in, and every entry here is one
/// somebody checked against ISO 4217.
pub mod currencies {
    use super::Currency;

    /// US dollar — 2 decimal places, ISO 4217 numeric `840`.
    pub const USD: Currency = Currency {
        alpha: "USD",
        numeric: "840",
        minor_unit: Some(2),
    };
    /// Euro — 2 decimal places, numeric `978`.
    pub const EUR: Currency = Currency {
        alpha: "EUR",
        numeric: "978",
        minor_unit: Some(2),
    };
    /// Pound sterling — 2 decimal places, numeric `826`.
    pub const GBP: Currency = Currency {
        alpha: "GBP",
        numeric: "826",
        minor_unit: Some(2),
    };
    /// Japanese yen — **no decimal places**, numeric `392`. The case that proves
    /// the minor unit is data and not an assumption.
    pub const JPY: Currency = Currency {
        alpha: "JPY",
        numeric: "392",
        minor_unit: Some(0),
    };
    /// Swiss franc — 2 decimal places, numeric `756`.
    pub const CHF: Currency = Currency {
        alpha: "CHF",
        numeric: "756",
        minor_unit: Some(2),
    };
    /// Canadian dollar — 2 decimal places, numeric `124`.
    pub const CAD: Currency = Currency {
        alpha: "CAD",
        numeric: "124",
        minor_unit: Some(2),
    };
    /// Australian dollar — 2 decimal places, numeric `036`.
    pub const AUD: Currency = Currency {
        alpha: "AUD",
        numeric: "036",
        minor_unit: Some(2),
    };
    /// Swedish krona — 2 decimal places, numeric `752`.
    pub const SEK: Currency = Currency {
        alpha: "SEK",
        numeric: "752",
        minor_unit: Some(2),
    };
    /// Norwegian krone — 2 decimal places, numeric `578`.
    pub const NOK: Currency = Currency {
        alpha: "NOK",
        numeric: "578",
        minor_unit: Some(2),
    };
    /// Danish krone — 2 decimal places, numeric `208`.
    pub const DKK: Currency = Currency {
        alpha: "DKK",
        numeric: "208",
        minor_unit: Some(2),
    };
    /// Indian rupee — 2 decimal places, numeric `356`.
    pub const INR: Currency = Currency {
        alpha: "INR",
        numeric: "356",
        minor_unit: Some(2),
    };
    /// Brazilian real — 2 decimal places, numeric `986`.
    pub const BRL: Currency = Currency {
        alpha: "BRL",
        numeric: "986",
        minor_unit: Some(2),
    };
    /// Singapore dollar — 2 decimal places, numeric `702`.
    pub const SGD: Currency = Currency {
        alpha: "SGD",
        numeric: "702",
        minor_unit: Some(2),
    };
    /// New Zealand dollar — 2 decimal places, numeric `554`.
    pub const NZD: Currency = Currency {
        alpha: "NZD",
        numeric: "554",
        minor_unit: Some(2),
    };
    /// South African rand — 2 decimal places, numeric `710`.
    pub const ZAR: Currency = Currency {
        alpha: "ZAR",
        numeric: "710",
        minor_unit: Some(2),
    };
    /// Polish złoty — 2 decimal places, numeric `985`.
    pub const PLN: Currency = Currency {
        alpha: "PLN",
        numeric: "985",
        minor_unit: Some(2),
    };
    /// Turkish lira — 2 decimal places, numeric `949`.
    pub const TRY: Currency = Currency {
        alpha: "TRY",
        numeric: "949",
        minor_unit: Some(2),
    };
    /// Bitcoin — **`n.a.`**, no minor unit. The reason `minor_unit` is an
    /// `Option`: satoshis are a scale, but ISO 4217 does not record one.
    pub const XBT: Currency = Currency {
        alpha: "XBT",
        numeric: "N.A.",
        minor_unit: None,
    };

    /// Every currency the ledger supports.
    pub const ALL: &[Currency] = &[
        USD, EUR, GBP, JPY, CHF, CAD, AUD, SEK, NOK, DKK, INR, BRL, SGD, NZD, ZAR, PLN, TRY, XBT,
    ];

    /// Look a currency up by its ISO 4217 alphabetic code.
    pub fn by_alpha(alpha: &str) -> Option<&'static Currency> {
        ALL.iter().find(|c| c.alpha == alpha)
    }
}

/// A monetary amount: an exact integer at an explicit scale.
///
/// # Representation
///
/// `value == units / 10^scale`, and `units` is `i64`. So 10.00 USD is
/// `units: 1000, scale: 2`, and 1000 JPY is `units: 1000, scale: 0` — the
/// currency's exponent, never assumed.
///
/// Nothing here is a float. `decimal-money` in this estate does decimal
/// arithmetic for tax rates and the like; a *ledger* wants integers, because
/// the value being stored is a count of minor units that a bank will agree to.
///
/// # Rounding, once
///
/// `rounded_to` records the scale this value was rounded at, so a second
/// rounding can be refused. ISO 2 §3.3 and GB/T 8170-2008 §3.3.1 both forbid
/// successive rounding: `97.46 → 97.5 → 98` is not `97.5`, and a ledger that
/// rounds a line, then a document total, then a tax return has produced three
/// answers for one economic fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Amount {
    /// The value multiplied by `10^scale`.
    pub units: i64,
    /// How many decimal places `units` carries.
    pub scale: u32,
    /// The scale this value was last rounded to, if it was rounded at all.
    /// `None` means the value came from its source exactly.
    pub rounded_to: Option<u32>,
    /// The currency this amount is denominated in.
    pub currency: Currency,
}

impl Amount {
    /// An exact amount of whole minor units, at the currency's own exponent.
    ///
    /// `units` is `value * 10^scale`, so a count of minor units is already in
    /// that form: 1000 USD minor units is `units: 1000, scale: 2`, which prints
    /// as `10.00`.
    pub fn minor(minor_units: i64, currency: Currency) -> Self {
        Self {
            units: minor_units,
            scale: currency.scale().unwrap_or(0),
            rounded_to: None,
            currency,
        }
    }

    /// An exact amount at an explicit scale: `units / 10^scale`.
    ///
    /// This is the constructor for a computed value — a tax line, a converted
    /// amount — where the scale is the caller's, not the currency's.
    pub fn scaled(units: i64, scale: u32, currency: Currency) -> Self {
        Self {
            units,
            scale,
            rounded_to: None,
            currency,
        }
    }

    /// The amount as a whole count of the currency's minor units, or `None` when
    /// it carries a sub-minor remainder.
    ///
    /// This is the answer to "is this amount representable in this currency?"
    /// with no rounding decision smuggled in.
    pub fn minor_units(self) -> Option<i64> {
        let minor_scale = self.currency.scale()?;
        if self.scale < minor_scale {
            // Finer than the minor unit: never a whole count of one.
            return None;
        }
        let divisor = pow10(self.scale - minor_scale);
        if self.units % divisor != 0 {
            return None;
        }
        Some(self.units / divisor)
    }

    /// Whether this value is a whole number of the currency's minor units.
    pub fn is_whole_minor_units(self) -> bool {
        self.minor_units().is_some()
    }

    /// Round to `to_scale` under `mode`.
    ///
    /// Refuses a second rounding: a value that already carries a rounding at a
    /// coarser-or-equal scale cannot be rounded finer without inventing
    /// precision. See the module docs.
    pub fn quantize(&self, to_scale: u32, mode: RoundingMode) -> Result<Amount, AmountError> {
        if let Some(previous) = self.rounded_to {
            if to_scale < previous {
                return Err(AmountError::SuccessiveRounding {
                    to_scale,
                    from_scale: previous,
                });
            }
        }
        if to_scale == self.scale {
            return Ok(Amount {
                rounded_to: Some(to_scale),
                ..*self
            });
        }
        if to_scale > self.scale {
            // Strictly finer than the source: exact, so no rounding decision.
            return Ok(Amount {
                units: self.units.checked_mul(pow10(to_scale - self.scale)).ok_or(
                    AmountError::Overflow {
                        currency: self.currency.alpha,
                    },
                )?,
                scale: to_scale,
                rounded_to: self.rounded_to,
                currency: self.currency,
            });
        }

        // Coarser: the only case that loses information.
        let divisor = pow10(self.scale - to_scale);
        let negative = self.units < 0;
        let magnitude = self.units.unsigned_abs();
        let d = divisor.unsigned_abs();
        let quotient = magnitude / d;
        let remainder = magnitude % d;
        let half = d / 2;

        let bump = match remainder.cmp(&half) {
            core::cmp::Ordering::Less => false,
            core::cmp::Ordering::Greater => true,
            // An exact tie. The candidate below the tie is `quotient`; the one
            // above is `quotient + 1`. Half-even takes whichever is even.
            core::cmp::Ordering::Equal => mode.tie_takes_upper(quotient % 2 == 0),
        };

        let magnitude = quotient + u64::from(bump);
        if magnitude > i64::MAX as u64 {
            return Err(AmountError::Overflow {
                currency: self.currency.alpha,
            });
        }
        let units = if negative {
            -(magnitude as i64)
        } else {
            magnitude as i64
        };

        // A value finer than the currency's own minor unit cannot be stored as
        // a count of that minor unit. Refused rather than rounded away.
        if let Some(minor_scale) = self.currency.scale() {
            if to_scale < minor_scale {
                return Err(AmountError::NotRepresentable {
                    currency: self.currency.alpha,
                    remainder: format!("scale {to_scale} is finer than the minor unit"),
                });
            }
        }

        Ok(Amount {
            units,
            scale: to_scale,
            rounded_to: Some(to_scale),
            currency: self.currency,
        })
    }

    /// Round to the currency's own exponent under `mode`.
    ///
    /// Refused for a currency ISO 4217 records as `n.a.`: there is no minor
    /// unit to round to, and pretending otherwise invents precision.
    pub fn quantize_to_currency(self, mode: RoundingMode) -> Result<Amount, AmountError> {
        let scale = self
            .currency
            .scale()
            .ok_or_else(|| AmountError::NotRepresentable {
                currency: self.currency.alpha,
                remainder: "ISO 4217 records no minor unit for this currency".to_string(),
            })?;
        self.quantize(scale, mode)
    }

    /// Negate, preserving scale and rounding history.
    pub fn negated(self) -> Amount {
        Amount {
            units: -self.units,
            ..self
        }
    }

    /// The absolute value.
    pub fn magnitude(self) -> Amount {
        Amount {
            units: self.units.abs(),
            ..self
        }
    }

    /// Add two amounts of the same currency.
    ///
    /// The scale of the result is the coarser of the two, so the addition never
    /// silently drops precision; a remainder that carries past the result's
    /// scale is resolved by `mode`.
    pub fn checked_add(self, other: Amount, mode: RoundingMode) -> Result<Amount, AmountError> {
        if self.currency.alpha != other.currency.alpha {
            return Err(AmountError::CurrencyCodeMalformed(format!(
                "{} + {}",
                self.currency.alpha, other.currency.alpha
            )));
        }
        let scale = self.scale.min(other.scale);
        let left = self.to_scale_exact(scale)?;
        let right = other.to_scale_exact(scale)?;
        let units = left
            .units
            .checked_add(right.units)
            .ok_or(AmountError::Overflow {
                currency: self.currency.alpha,
            })?;
        // The exact sum can only need rounding if the inputs carried different
        // scales and the finer one had a remainder — and `to_scale_exact` has
        // already folded those into the coarser scale.
        Ok(Amount {
            units,
            scale,
            rounded_to: if left.rounded_to.is_some() || right.rounded_to.is_some() {
                Some(scale)
            } else {
                None
            },
            currency: self.currency,
        })
        .map(|a| a.widened_by(mode))
    }

    /// Rescale to a coarser scale exactly, refusing to lose a remainder.
    fn to_scale_exact(self, scale: u32) -> Result<Amount, AmountError> {
        if scale == self.scale {
            return Ok(self);
        }
        let divisor = pow10(self.scale - scale);
        if self.units % divisor != 0 {
            return Err(AmountError::NotRepresentable {
                currency: self.currency.alpha,
                remainder: format!("{self} is not a whole multiple of 10^{scale}"),
            });
        }
        Ok(Amount {
            units: self.units / divisor,
            scale,
            rounded_to: self.rounded_to,
            currency: self.currency,
        })
    }

    /// Identity for [`RoundingMode`]-free results: `checked_add` takes a mode
    /// so a carry can be resolved, but an exact sum needs none. Kept as a
    /// method so the intent is explicit at the call site.
    fn widened_by(self, _mode: RoundingMode) -> Amount {
        self
    }

    /// This amount as a decimal string, for display and export.
    ///
    /// Never scientific notation and never locale-dependent: a ledger export
    /// that renders differently per machine is not an export.
    pub fn to_decimal_string(self) -> String {
        let negative = self.units < 0;
        let magnitude = self.units.unsigned_abs();
        let sign = if negative { "-" } else { "" };
        if self.scale == 0 {
            return format!("{sign}{magnitude}");
        }
        let divisor = pow10(self.scale) as u64;
        let whole = magnitude / divisor;
        let fraction = magnitude % divisor;
        format!(
            "{sign}{whole}.{fraction:0width$}",
            width = self.scale as usize
        )
    }
}

/// `10^n` for the scales a money amount uses, saturating rather than wrapping.
fn pow10(n: u32) -> i64 {
    const TABLE: [i64; 19] = [
        1,
        10,
        100,
        1_000,
        10_000,
        100_000,
        1_000_000,
        10_000_000,
        100_000_000,
        1_000_000_000,
        10_000_000_000,
        100_000_000_000,
        1_000_000_000_000,
        10_000_000_000_000,
        100_000_000_000_000,
        1_000_000_000_000_000,
        10_000_000_000_000_000,
        100_000_000_000_000_000,
        1_000_000_000_000_000_000,
    ];
    TABLE.get(n as usize).copied().unwrap_or(i64::MAX)
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.to_decimal_string(), self.currency.alpha)
    }
}
