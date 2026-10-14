//! Explicit metadata binding for a dedicated, authenticated Noah forwarder.

use std::str::FromStr;

use anyhow::{ensure, Context};
use bitcoin::hashes::{sha256, Hash};

const PREFIX: &str = "Paying satoshis to ";

#[derive(Clone, Debug)]
pub struct NoahLnurl {
	domain: String,
}

impl FromStr for NoahLnurl {
	type Err = anyhow::Error;

	fn from_str(domain: &str) -> Result<Self, Self::Err> {
		ensure!(!domain.is_empty() && domain.len() <= 253, "invalid Noah LNURL domain length");
		ensure!(domain.split('.').all(|label| {
			!label.is_empty() && label.len() <= 63
				&& label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
				&& label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
				&& label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
		}), "Noah LNURL domain must be an exact lowercase DNS name");
		Ok(Self { domain: domain.to_owned() })
	}
}

impl NoahLnurl {
	/// Preserve the exact metadata bytes; the label is not a recipient ownership proof.
	pub fn description_hash(&self, description: &str) -> anyhow::Result<sha256::Hash> {
		ensure!(description.len() <= PREFIX.len() + 255 + 1 + self.domain.len(), "Noah invoice description is too long");
		let identifier = description.strip_prefix(PREFIX).context("unexpected Noah invoice description")?;
		let (username, domain) = identifier.split_once('@').context("Noah identifier is missing its domain")?;
		ensure!(domain == self.domain, "Noah invoice domain does not match the configured domain");
		ensure!(!username.is_empty() && username.len() <= 255
			&& username.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')),
			"invalid Noah LNURL username");
		let metadata = serde_json::to_vec(&[["text/identifier", identifier], ["text/plain", description]])?;
		Ok(sha256::Hash::hash(&metadata))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hash_matches_actual_ordered_metadata() {
		let mode: NoahLnurl = "owned.invalid".parse().unwrap();
		assert_eq!(mode.description_hash("Paying satoshis to forwardedphone@owned.invalid").unwrap().to_string(),
			"c67173a75edfb17da584b044179ed38e0053f1da5c5e9f3f558f2bb518a0444b");
	}

	#[test]
	fn rejects_mismatches_without_normalizing() {
		let mode: NoahLnurl = "owned.invalid".parse().unwrap();
		for label in ["arbitrary memo", "Paying satoshis to name@other.invalid", "Paying satoshis to Name@owned.invalid",
			"Paying satoshis to name@owned.invalid ", "Paying satoshis to name@owned.invalid@owned.invalid",
			"Paying satoshis to @owned.invalid", "Paying satoshis to n\"ame@owned.invalid"] {
			assert!(mode.description_hash(label).is_err(), "accepted {label}");
		}
	}

	#[test]
	fn rejects_invalid_domain_configuration() {
		for domain in ["", "Owned.invalid", "https://owned.invalid", "owned.invalid:443", "owned.invalid/path", ".invalid", "owned..invalid", "-owned.invalid"] {
			assert!(domain.parse::<NoahLnurl>().is_err(), "accepted {domain}");
		}
	}
}
