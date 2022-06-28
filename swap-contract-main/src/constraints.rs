//! Various constraints as required for production environments

use crate::{
    curve::{
        base::{CurveType, SwapCurve},
        fees::Fees,
    },
    error::AmmError,
};

use solana_program::program_error::ProgramError;

/// Encodes fee constraints, used in multihost environments where the program
/// may be used by multiple frontends, to ensure that proper fees are being
/// assessed.
/// Since this struct needs to be created at compile-time, we only have access
/// to const functions and constructors. Since SwapCurve contains a Box, it
/// cannot be used, so we have to split the curves based on their types.
pub struct SwapConstraints<'a> {
    /// Owner of the program
    pub owner_key: &'a str,
    /// Valid curve types
    pub valid_curve_types: &'a [CurveType],
    /// Valid fees
    pub fees: &'a Fees,
}

impl<'a> SwapConstraints<'a> {
    /// Checks that the provided curve is valid for the given constraints
    pub fn validate_curve(&self, swap_curve: &SwapCurve) -> Result<(), ProgramError> {
        if self.valid_curve_types.iter().any(|x| *x == swap_curve.curve_type) &&
            self.valid_curve_types.iter().any(|x| *x == swap_curve.calculator.get_curve_type())
        {
            Ok(())
        } else {
            Err(AmmError::UnsupportedCurveType.into())
        }
    }

    /// Checks that the provided curve is valid for the given constraints
    pub fn validate_fees(&self, fees: &Fees) -> Result<(), ProgramError> {
        if fees.return_fee_numerator >= self.fees.return_fee_numerator
            && fees.fixed_fee_numerator >= self.fees.fixed_fee_numerator
            && fees.fee_denominator == self.fees.fee_denominator
        {
            Ok(())
        } else {
            Err(AmmError::InvalidFee.into())
        }
    }
}

// const OWNER_KEY: &str = env!("SWAP_PROGRAM_OWNER_FEE_ADDRESS");
// const OWNER_KEY: &str = "AMMAE3eViwHuH25gWHfLpsVqtwmBSksGohE53oEmYrG2";
const OWNER_KEY: &str = "DjXkZxNWUoGsL87rbWRFVPmoxN1FKXUWpinUyN921PwQ";

const FEES: &Fees = &Fees {
    fixed_fee_numerator: 20,
    return_fee_numerator: 10,
    fee_denominator: 10000,
};
const VALID_CURVE_TYPES: &[CurveType] = &[CurveType::ConstantProduct];

/// Fee structure defined by program creator in order to enforce certain
/// fees when others use the program.  Adds checks on pool creation and
/// swapping to ensure the correct fees and account owners are passed.
/// Fees provided during production build currently are considered min
/// fees that creator of the pool can specify. Host fee is a fixed
/// percentage that host receives as a portion of owner fees
pub const SWAP_CONSTRAINTS:SwapConstraints = SwapConstraints {
    owner_key: OWNER_KEY,
    valid_curve_types: VALID_CURVE_TYPES,
    fees: FEES,
};