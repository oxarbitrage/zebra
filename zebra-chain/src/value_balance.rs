//! A type that contains the Zcash four pools (transparent, sprout, sapling, orchard)
//!
//! The [`Amount`] type is parameterized by a [`Constraint`] implementation that
//! declares the range of allowed values. In contrast to regular arithmetic
//! operations, which return values, arithmetic on [`Amount`]s returns
//! [`Result`](std::result::Result)s.

use crate::amount::{Amount, Constraint, Error, NegativeAllowed, NonNegative};

//use itertools::Itertools;

/// Document the struct
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(bound = "C: Constraint")]
pub struct ValueBalance<C = NegativeAllowed> {
    transparent: Amount<C>,
    sprout: Amount<C>,
    sapling: Amount<C>,
    orchard: Amount<C>,
}

impl<C> ValueBalance<C>
where
    C: Constraint + Copy,
{
    /// [Consensus rule]: The remaining value in the transparent transaction value pool MUST be nonnegative.
    ///
    /// This rule applies to Block and Mempool transactions.
    ///
    /// [Consensus rule]: https://zips.z.cash/protocol/protocol.pdf#transactions
    pub fn remaining_transaction_value(&self) -> Result<Amount<NonNegative>, Error> {
        // This rule checks the transparent value balance minus the sum of the sprout, sapling, and orchard
        // value balances in a transaction is nonnegative
        Ok((self.transparent - (self.sprout + self.sapling + self.orchard)?)?.constrain::<NonNegative>()?)
    }

    /// Create a new ValueBalance
    pub fn new(
        transparent: Option<Amount<C>>,
        sprout: Option<Amount<C>>,
        sapling: Option<Amount<C>>,
        orchard: Option<Amount<C>>,
    ) -> ValueBalance<C> {

        let mut result = ValueBalance::default();

        if let Some(transparent) = transparent {
            result.transparent = transparent;
        }
        if let Some(sprout) = sprout {
            result.sprout = sprout;
        }
        if let Some(sapling) = sapling {
            result.sapling = sapling;
        }
        if let Some(orchard) = orchard {
            result.orchard = orchard;
        }
        result
    }
}

/*
impl<C> std::ops::Add<ValueBalance<C>> for Result<ValueBalance<C>, Error>
where
    C: Constraint,
{
    type Output = Result<ValueBalance<C>, Error>;

    fn add(self, rhs: ValueBalance<C>) -> Self::Output {
        let vb = self?;

        let sum = ValueBalance::<C> {
            transparent: (vb.transparent + rhs.transparent).unwrap(),
            sprout: (vb.sprout + rhs.sprout).unwrap(),
            sapling: (vb.sapling + rhs.sapling).unwrap(),
            orchard: (vb.orchard + rhs.orchard).unwrap(),
        };
        Ok(sum)
    }
}

impl<C> std::ops::Sub<ValueBalance<C>> for Result<ValueBalance<C>, Error>
where
    C: Constraint,
{
    type Output = Result<ValueBalance<C>, Error>;

    fn sub(self, rhs: ValueBalance<C>) -> Self::Output {
        let vb = self?;

        let sub = ValueBalance::<C> {
            transparent: (vb.transparent - rhs.transparent).unwrap(),
            sprout: (vb.sprout - rhs.sprout).unwrap(),
            sapling: (vb.sapling - rhs.sapling).unwrap(),
            orchard: (vb.orchard - rhs.orchard).unwrap(),
        };
        Ok(sub)
    }
}
*/

/*
impl AddAssign for Result<ValueBalance<C>>
where
    C: Constraint,
{

}

impl SubAssign for Result<ValueBalance<C>>
where
    C: Constraint,
{

}

impl Sum for Result<ValueBalance<C>>
where
    C: Constraint,
{

}

*/
use std::convert::TryFrom;
impl<C> Default for ValueBalance<C>
where
    C: Constraint + Copy,
{
    fn default() -> Self {
        let zero = Amount::try_from(0).expect("an amount of 0 is always valid");
        Self {
            transparent: zero,
            sprout: zero,
            sapling: zero,
            orchard: zero,
        }
    }

}