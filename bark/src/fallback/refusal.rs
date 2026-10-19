//! Stable server refusals, independent of diagnostic message text.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum FallbackRefusal {
	#[error("This coin expired and is being paid to your fallback address.")]
	DecidedForFallback,
	#[error("This coin expired. Refresh it first, then send.")]
	ExpiredNewLightning,
}

impl FallbackRefusal {
	pub fn code(self) -> &'static str {
		match self {
			Self::DecidedForFallback => "decided_for_fallback",
			Self::ExpiredNewLightning => "expired_new_lightning",
		}
	}

	pub fn from_status(status: &tonic::Status) -> Option<Self> {
		if status.code() != tonic::Code::InvalidArgument {
			return None;
		}
		match status.metadata().get("abandon-reason")?.to_str().ok()? {
			"decided_for_fallback" => Some(Self::DecidedForFallback),
			"expired_new_lightning" => Some(Self::ExpiredNewLightning),
			_ => None,
		}
	}

	/// Classify a refusal through SDK context and action error wrappers.
	pub fn from_error(error: &anyhow::Error) -> Option<Self> {
		if let Some(refusal) = error.downcast_ref::<Self>() { return Some(*refusal); }
		for cause in error.chain() {
			if let Some(refusal) = cause.downcast_ref::<Self>() {
				return Some(*refusal);
			}
			if let Some(status) = cause.downcast_ref::<tonic::Status>() {
				if let Some(refusal) = Self::from_status(status) { return Some(refusal); }
			}
		}
		None
	}

	pub(crate) fn map_status(status: tonic::Status) -> anyhow::Error {
		match Self::from_status(&status) {
			Some(refusal) => anyhow::Error::new(status).context(refusal),
			None => status.into(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn typed_anyhow_context_without_tonic_is_classified() {
		for refusal in [FallbackRefusal::DecidedForFallback, FallbackRefusal::ExpiredNewLightning] {
			let error = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
				.context(refusal).context("outer action context");
			assert!(error.downcast_ref::<tonic::Status>().is_none());
			assert_eq!(FallbackRefusal::from_error(&error), Some(refusal));
			assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().kind(), std::io::ErrorKind::PermissionDenied);
		}
	}

	#[test]
	fn metadata_alone_selects_the_typed_refusal_and_preserves_original_status() {
		for refusal in [FallbackRefusal::DecidedForFallback, FallbackRefusal::ExpiredNewLightning] {
			let mut status = tonic::Status::invalid_argument("server diagnostic text can change");
			status.metadata_mut().insert("abandon-reason", refusal.code().parse().unwrap());
			let error = FallbackRefusal::map_status(status).context("operation context");
			assert_eq!(FallbackRefusal::from_error(&error), Some(refusal));
			assert_eq!(error.downcast_ref::<FallbackRefusal>(), Some(&refusal));
			assert_eq!(error.downcast_ref::<tonic::Status>().unwrap().code(), tonic::Code::InvalidArgument);
		}
	}

	#[test]
	fn diagnostic_text_unknown_codes_and_transport_errors_do_not_become_refusals() {
		let status = tonic::Status::invalid_argument(FallbackRefusal::DecidedForFallback.to_string());
		assert_eq!(FallbackRefusal::from_status(&status), None);
		let mut status = tonic::Status::invalid_argument("unknown future refusal");
		status.metadata_mut().insert("abandon-reason", "future_reason".parse().unwrap());
		assert_eq!(FallbackRefusal::from_status(&status), None);
		let mut status = tonic::Status::unavailable("connection failed");
		status.metadata_mut().insert("abandon-reason", "decided_for_fallback".parse().unwrap());
		assert_eq!(FallbackRefusal::from_status(&status), None);
	}
}
