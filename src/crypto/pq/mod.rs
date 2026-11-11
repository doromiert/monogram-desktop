// SPDX-License-Identifier: AGPL-3.0-only
//! Monocles PQ OMEMO profile. Never shares identities, sessions or records with classic OMEMO.
pub mod bundle;
pub mod device;
pub mod native;
pub mod payload;
pub mod pep;
pub mod service;
pub mod store;
pub mod wire;
pub use crate::crypto::{Error, Result, Secret, address};
pub const NS: &str = "urn:monocles:omemo-pq:1";
