use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
	pub port: u16,
	pub host: IpAddr,
	pub allowed_origins: Vec<String>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			port: 3000,
			host: "127.0.0.1".parse().expect("Invalid ip address"),
			allowed_origins: Vec::new(),
		}
	}
}

impl Config {
	pub fn socket_addr(&self) -> SocketAddr {
		SocketAddr::from((self.host, self.port))
	}
}
/// Read one operator-selected public root certificate before serving requests.
/// Payment requests cannot select paths, roots or transport configuration.
pub fn read_lnurl_root_ca_pem(path: Option<&std::path::Path>) -> anyhow::Result<Option<Vec<u8>>> {
	use std::io::Read;
	let Some(path) = path else { return Ok(None); };
	let file = std::fs::File::open(path)?;
	anyhow::ensure!(file.metadata()?.is_file(), "LNURL root CA must be a regular public PEM file");
	let mut pem = Vec::new();
	file.take(65537).read_to_end(&mut pem)?;
	validate_lnurl_root_ca_pem(&pem)?;
	Ok(Some(pem))
}

pub(crate) fn validate_lnurl_root_ca_pem(pem: &[u8]) -> anyhow::Result<()> {
	anyhow::ensure!(!pem.is_empty() && pem.len() <= 65536, "LNURL root CA PEM exceeds its 64 KiB bound or is empty");
	let certificate = reqwest_lnurl::Certificate::from_pem(pem)?;
	// Building the TLS root store also rejects a PEM wrapper around invalid DER.
	let _client = reqwest_lnurl::Client::builder()
		.https_only(true)
		.redirect(reqwest_lnurl::redirect::Policy::none())
		.timeout(std::time::Duration::from_secs(30))
		.add_root_certificate(certificate)
		.build()?;
	Ok(())
}

#[cfg(test)]
mod lnurl_root_ca_tests {
	use super::*;

	#[test]
	fn absent_root_preserves_native_system_roots() {
		assert!(read_lnurl_root_ca_pem(None).unwrap().is_none());
	}

	#[test]
	fn public_pem_is_parsed_and_frozen_after_one_read() {
		let public = include_bytes!("../tests/fixtures/isrg-root-x1.pem");
		let path = std::env::temp_dir().join(format!("bark-rest-lnurl-root-{}", std::process::id()));
		let mut file = std::fs::OpenOptions::new().create_new(true).write(true).open(&path).unwrap();
		std::io::Write::write_all(&mut file, public).unwrap();
		file.sync_all().unwrap();
		let frozen = read_lnurl_root_ca_pem(Some(&path)).unwrap().unwrap();
		std::fs::write(&path, b"invalid replacement").unwrap();
		assert_eq!(frozen, public);
		assert!(read_lnurl_root_ca_pem(Some(&path)).is_err());
		std::fs::remove_file(path).unwrap();
	}

	#[test]
	fn malformed_empty_and_oversized_public_roots_refuse() {
		for pem in [Vec::new(), b"not a public certificate".to_vec(),
			b"-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n".to_vec(), vec![0; 65537]] {
			assert!(validate_lnurl_root_ca_pem(&pem).is_err());
		}
	}
}
