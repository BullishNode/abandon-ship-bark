//! Immutable expiry destinations and individually signed coin ownership.

pub use crate::per_key::{Error as FallbackError, FallbackRecord, KeyLink};

use bitcoin::secp256k1::PublicKey;

/// Mailbox provenance resolved from a retained individual key signature.
/// This is internal admission context, not a derivation or spending authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyLinkOrigin {
	mailbox_pk: PublicKey,
}

impl KeyLinkOrigin {
	pub fn new(mailbox_pk: PublicKey) -> Self { Self { mailbox_pk } }
	pub fn mailbox_pk(&self) -> PublicKey { self.mailbox_pk }
}
