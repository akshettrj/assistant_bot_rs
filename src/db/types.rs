//! Column types shared by the entities.

use std::{fmt, str::FromStr};

use rust_decimal::Decimal;
use sea_orm::DeriveValueType;
use serde::{Deserialize, Serialize};

/// An exact decimal, stored as its canonical string (`12.50`): SQLite has no
/// exact decimal type, and SeaORM's would go through a float.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, DeriveValueType)]
#[sea_orm(value_type = "String")]
#[serde(transparent)]
pub struct Dec(pub Decimal);

impl FromStr for Dec {
    type Err = rust_decimal::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Decimal::from_str_exact(text).map(Dec)
    }
}

impl fmt::Display for Dec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Decimal> for Dec {
    fn from(value: Decimal) -> Self {
        Dec(value)
    }
}
