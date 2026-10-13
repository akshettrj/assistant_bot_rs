//! Exporting a trip to a Splitwise group, for everyone there to see.
//!
//! Each entry becomes one Splitwise expense (a settlement, a payment) with
//! the trip's exact shares, the receipt's items in its notes ([`payload`]).
//! The trip's people are matched with Splitwise users ([`people`]). An export
//! adds, changes and deletes only the expenses the bot added ([`plan`]), and
//! can be run again at any time ([`service`]). The pages are in [`panel`].

pub mod panel;
pub mod payload;
pub mod people;
pub mod plan;
pub mod service;
