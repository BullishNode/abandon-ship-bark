
mod pay;
mod receive;

pub use self::pay::LightningSendOptions;

use anyhow::Context;
use bitcoin::{Amount, Network};
use bitcoin::hashes::Hash;
use lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescriptionRef, Currency};
use lnurllib::LnUrlResponse;
use lnurllib::lightning_address::LightningAddress;
use lnurllib::pay::PayResponse;
use log::warn;

use bitcoin_ext::AmountExt;

/// Verifies that a BOLT11 invoice returned by an LNURL-pay endpoint
/// matches the request we made, since the endpoint is untrusted.
fn verify_lnurlp_invoice(
	invoice: &Bolt11Invoice,
	amount: Amount,
	resp: &PayResponse,
	require_metadata: bool,
) -> anyhow::Result<()> {
	if require_metadata && invoice.is_expired() {
		bail!("strict lnurl invoice has expired");
	}
	match invoice.amount_milli_satoshis() {
		Some(msat) if msat == amount.to_msat() => {},
		Some(msat) => bail!(
			"lnurl-pay endpoint returned an invoice for {} msat, \
			but we requested {} ({} msat)",
			msat, amount, amount.to_msat(),
		),
		None => bail!(
			"lnurl-pay endpoint returned an amountless invoice, \
			but we requested {}", amount,
		),
	}

	// nb this is no longer a hard requirement in LUD-06 spec
	let metadata_bound = match invoice.description() {
		Bolt11InvoiceDescriptionRef::Hash(h) => h.0.to_byte_array() == resp.metadata_hash(),
		Bolt11InvoiceDescriptionRef::Direct(_) => false,
	};
	if !metadata_bound {
		if require_metadata {
			bail!("lnurl-pay invoice does not commit to the exact advertised metadata");
		}
		warn!("lnurl-pay invoice description hash does not commit to the endpoint metadata");
	}

	Ok(())
}

/// Performs an LNURL-Pay request against `lnurlp` and returns the BOLT11
/// invoice the endpoint issues for the requested amount.
///
/// `comment` is forwarded as the LNURL-Pay `comment` field when present; the
/// endpoint may reject or truncate it according to its own `commentAllowed`
/// limit.
///
/// Returns an error if the endpoint resolves to a non-pay LNURL response
/// (withdraw, channel), if the HTTP request fails, if the returned bolt11
/// string is not a valid invoice, or if the invoice's amount doesn't match
/// the requested amount.
pub(crate) async fn lnurlp_invoice(
	lnurlp: &str,
	amount: Amount,
	comment: Option<impl AsRef<str>>,
	opts: &LightningSendOptions,
	expected_identifier: Option<&str>,
) -> anyhow::Result<Bolt11Invoice> {
	let strict = opts.require_lnurl_metadata;
	let client = if strict {
		strict_lnurl_client(opts)?
	} else {
		lnurllib::Builder::default().build_async().context("lnurl client error")?
	};
	let response = if strict {
		let endpoint = strict_lnurl_endpoint(lnurlp)?;
		lnurllib::decode_ln_url_response(&strict_lnurl_request(&client, endpoint).await?)?
	} else {
		client.make_request(lnurlp).await.context("failed to make lnurl request")?
	};
	let resp = match response {
		LnUrlResponse::LnUrlPayResponse(v) => v,
		LnUrlResponse::LnUrlWithdrawResponse(_) => bail!("received lnurl withdraw"),
		LnUrlResponse::LnUrlChannelResponse(_) => bail!("received lnurl channel"),
	};
	let comment = comment.as_ref().map(|s| s.as_ref());
	let invoice = if strict {
		verify_strict_lnurl_endpoint(lnurlp, &resp, expected_identifier)?;
		let msat = amount.to_sat().checked_mul(1_000).context("lnurl amount overflows millisatoshis")?;
		if msat < resp.min_sendable || msat > resp.max_sendable {
			bail!("strict lnurl amount outside advertised range");
		}
		if let (Some(comment), Some(max)) = (comment, resp.comment_allowed) {
			if comment.len() > max as usize { bail!("strict lnurl comment exceeds advertised limit"); }
		}
		let mut callback = strict_lnurl_endpoint(&resp.callback)?;
		callback.query_pairs_mut().append_pair("amount", &msat.to_string());
		if let Some(comment) = comment { callback.query_pairs_mut().append_pair("comment", comment); }
		let invoice: lnurllib::pay::LnURLPayInvoice = serde_json::from_str(
			&strict_lnurl_request(&client, callback).await?,
		)?;
		invoice.pr
	} else {
		client.get_invoice(&resp, amount.to_msat(), None, comment).await
			.context("failed to fetch invoice from lnurlpay")?.pr
	};
	let invoice: Bolt11Invoice = invoice.parse()
		.with_context(|| format!("received invalid invoice: {}", invoice))?;
	verify_lnurlp_invoice(&invoice, amount, &resp, strict)?;
	Ok(invoice)
}

// Both metadata and invoice responses fit within this explicit native envelope.
#[cfg(not(target_arch = "wasm32"))]
const STRICT_LNURL_RESPONSE_LIMIT: usize = 64 * 1024;

fn strict_lnurl_client(opts: &LightningSendOptions) -> anyhow::Result<lnurllib::AsyncClient> {
	#[cfg(not(target_arch = "wasm32"))]
	{
		let mut builder = reqwest_lnurl::Client::builder()
			.https_only(true)
			.redirect(reqwest_lnurl::redirect::Policy::none())
			.timeout(std::time::Duration::from_secs(30));
		if let Some(pem) = opts.lnurl_root_ca_pem.as_ref() {
			if pem.len() > STRICT_LNURL_RESPONSE_LIMIT { bail!("strict lnurl root certificate exceeds size limit"); }
			builder = builder.add_root_certificate(reqwest_lnurl::Certificate::from_pem(pem)?);
		}
		Ok(lnurllib::AsyncClient::from_client(builder.build()?))
	}
	#[cfg(target_arch = "wasm32")]
	{
		let _ = opts;
		bail!("strict lnurl transport requires the native client");
	}
}

#[cfg(not(target_arch = "wasm32"))]
fn append_strict_lnurl_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> anyhow::Result<()> {
	if body.len() > STRICT_LNURL_RESPONSE_LIMIT || chunk.len() > STRICT_LNURL_RESPONSE_LIMIT - body.len() {
		bail!("strict lnurl response exceeds size limit");
	}
	body.extend_from_slice(chunk);
	Ok(())
}

async fn strict_lnurl_request(client: &lnurllib::AsyncClient, endpoint: url::Url) -> anyhow::Result<String> {
	#[cfg(not(target_arch = "wasm32"))]
	{
		let response = client.client.get(endpoint.clone()).send().await.context("strict lnurl HTTPS request failed")?;
		if response.status().is_redirection() || response.url() != &endpoint {
			bail!("strict lnurl endpoint redirected");
		}
		let mut response = response.error_for_status()?;
		if response.content_length().is_some_and(|len| len > STRICT_LNURL_RESPONSE_LIMIT as u64) {
			bail!("strict lnurl response exceeds size limit");
		}
		let mut body = Vec::new();
		while let Some(chunk) = response.chunk().await? {
			append_strict_lnurl_chunk(&mut body, &chunk)?;
		}
		Ok(String::from_utf8(body).context("strict lnurl response is not UTF-8")?)
	}
	#[cfg(target_arch = "wasm32")]
	{
		let _ = (client, endpoint);
		bail!("strict lnurl transport requires the native client");
	}
}

fn strict_lnurl_endpoint(endpoint: &str) -> anyhow::Result<url::Url> {
	let endpoint = url::Url::parse(endpoint).context("invalid strict lnurl endpoint")?;
	if endpoint.scheme() != "https" || !endpoint.username().is_empty()
		|| endpoint.password().is_some() || endpoint.fragment().is_some() {
		bail!("strict lnurl requires an HTTPS endpoint without credentials or fragment");
	}
	Ok(endpoint)
}

fn verify_strict_lnurl_endpoint(
	requested: &str,
	resp: &PayResponse,
	expected_identifier: Option<&str>,
) -> anyhow::Result<()> {
	if strict_lnurl_endpoint(requested)? != strict_lnurl_endpoint(&resp.callback)? {
		bail!("strict lnurl callback does not match the requested recipient endpoint");
	}
	let metadata: Vec<Vec<String>> = serde_json::from_str(&resp.metadata).context("invalid strict lnurl metadata")?;
	if metadata.iter().any(|entry| entry.len() != 2) { bail!("invalid strict lnurl metadata entry"); }
	if let Some(expected) = expected_identifier {
		let identifiers = metadata.iter().filter(|entry| entry[0] == "text/identifier")
			.map(|entry| entry[1].as_str()).collect::<Vec<_>>();
		if identifiers != vec![expected] { bail!("strict lnurl identifier does not match the requested address"); }
	}
	Ok(())
}

/// Preserve the library's existing network check, accepting CLN's shared TB
/// currency on signet/testnet4. Bitcoin chain identity still comes from Core.
pub(crate) fn bolt11_currency_matches_network(currency: Currency, network: Network) -> bool {
	matches!((currency, network),
		(Currency::Bitcoin, Network::Bitcoin) |
		(Currency::BitcoinTestnet, Network::Testnet | Network::Testnet4 | Network::Signet) |
		(Currency::Signet, Network::Signet) |
		(Currency::Regtest, Network::Regtest)
	)
}

/// Resolves a Lightning Address (`user@domain`) to its LNURL-Pay endpoint and
/// returns a BOLT11 invoice issued by that endpoint for the requested amount.
///
/// Thin wrapper over [`lnurlp_invoice`]; see it for the meaning of `comment`
/// and for the set of conditions under which this call can fail.
pub(crate) async fn lnaddr_invoice(
	addr: &LightningAddress,
	amount: Amount,
	comment: Option<impl AsRef<str>>,
	opts: &LightningSendOptions,
) -> anyhow::Result<Bolt11Invoice> {
	let lnurl = addr.lnurlp_url();
	let identifier = addr.to_string();
	Ok(lnurlp_invoice(&lnurl, amount, comment, opts, Some(&identifier)).await?)
}

#[cfg(test)]
mod test {
	use std::str::FromStr;
	use std::sync::Arc;

	use bitcoin::Network;
	use ark::lightning::{Bolt12Invoice, Bolt12InvoiceExt, Invoice};
	use lightning_invoice::Bolt11Invoice;
	use lnurllib::Tag;

	use crate::{Config, OpenWalletArgs, Wallet, WalletSeed};
	use crate::lock_manager::memory::MemoryLockManager;
	use crate::persist::adaptor::StorageAdaptorWrapper;

	use super::*;

	/// A known-valid BOLT11 invoice on signet for 100u (10,000 sat).
	const TEST_INVOICE_STR: &str = "lntbs100u1p5j0x82sp5d0rwfh7tgrrlwsegy9rx3tzpt36cqwjqza5x4wvcjxjzscfaf6jspp5d8q7354dg3p8h0kywhqq5dq984r8f5en98hf9ln85ug0w8fx6hhsdqqcqzpc9qyysgqyk54v7tpzprxll7e0jyvtxcpgwttzk84wqsfjsqvcdtq47zt2wssxsmtjhz8dka62mdnf9jafhu3l4cpyfnsx449v4wstrwzzql2w5qqs8uh7p";

	fn dummy_pay_response() -> PayResponse {
		PayResponse {
			callback: "https://example.com/callback".into(),
			max_sendable: 100_000_000_000,
			min_sendable: 1_000,
			tag: Tag::PayRequest,
			metadata: "[[\"text/plain\",\"test\"]]".into(),
			comment_allowed: None,
			allows_nostr: None,
			nostr_pubkey: None,
		}
	}

	#[test]
	fn lnurlp_invoice_amount_verification() {
		let invoice = Bolt11Invoice::from_str(TEST_INVOICE_STR).unwrap();
		let resp = dummy_pay_response();

		// the invoice is for exactly 10,000 sat
		verify_lnurlp_invoice(&invoice, Amount::from_sat(10_000), &resp, false).unwrap();

		// an invoice for any other amount must be rejected
		let err = verify_lnurlp_invoice(&invoice, Amount::from_sat(9_000), &resp, false).unwrap_err();
		assert!(err.to_string().contains("10000000 msat"), "err: {}", err);
		verify_lnurlp_invoice(&invoice, Amount::from_sat(11_000), &resp, false).unwrap_err();
	}

	#[allow(unused)] // just exists for compile check
	async fn pay_lightning_invoice_argument() {
		//! Check the different possible argument for pay_lightning_invoice

		let db = Arc::new(StorageAdaptorWrapper::new_memory());
		let w = Wallet::open(
			Network::Regtest,
			WalletSeed::new_from_seed(Network::Regtest, &[1; 64]),
			Config::network_default(Network::Regtest),
			OpenWalletArgs {
				lock_manager: Some(Box::new(MemoryLockManager::new())),
				persister: Some(db),
				..Default::default()
			},
		).await.unwrap();

		let bolt11 = Bolt11Invoice::from_str("").unwrap();
		w.pay_lightning_invoice(bolt11, None, false).await.unwrap();

		let bolt12 = Bolt12Invoice::from_str("").unwrap();
		w.pay_lightning_invoice(bolt12, None, false).await.unwrap();

		let string = format!("lnbc1..");
		w.pay_lightning_invoice(string, None, false).await.unwrap();

		let strr = "lnbc1..";
		w.pay_lightning_invoice(strr, None, false).await.unwrap();

		let invoice = Invoice::Bolt11("".parse().unwrap());
		w.pay_lightning_invoice(invoice, None, false).await.unwrap();
	}
}

#[cfg(test)]
mod strict_lnurl_tests {
	use bitcoin::hashes::{sha256, Hash};
	use bitcoin::secp256k1::{Message, Secp256k1, SecretKey};
	use lightning_invoice::{InvoiceBuilder, PaymentSecret};

	use super::*;

	fn response() -> PayResponse {
		PayResponse {
			callback: "https://wallet.test/.well-known/lnurlp/alice".into(),
			min_sendable: 1_000, max_sendable: 100_000_000, tag: lnurllib::Tag::PayRequest,
			metadata: "[[\"text/identifier\",\"alice@wallet.test\"],[\"text/plain\",\"exact bytes\"]]".into(),
			comment_allowed: None, allows_nostr: None, nostr_pubkey: None,
		}
	}

	fn signed_invoice(metadata: &str, amount_msat: u64, direct: bool) -> Bolt11Invoice {
		signed_invoice_for_currency(metadata, amount_msat, direct, Currency::BitcoinTestnet)
	}

	fn signed_invoice_for_currency(metadata: &str, amount_msat: u64, direct: bool, currency: Currency) -> Bolt11Invoice {
		let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
		signed_invoice_at(metadata, amount_msat, direct, currency, now)
	}

	fn signed_invoice_at(metadata: &str, amount_msat: u64, direct: bool, currency: Currency, timestamp: std::time::Duration) -> Bolt11Invoice {
		let key = SecretKey::from_slice(&[2; 32]).unwrap();
		let secp = Secp256k1::new();
		let builder = InvoiceBuilder::new(currency);
		let builder = if direct { builder.description("direct memo".into()) }
			else { builder.description_hash(sha256::Hash::hash(metadata.as_bytes())) };
		builder.payment_hash(sha256::Hash::hash(b"test-only preimage"))
			.payment_secret(PaymentSecret([42; 32]))
			.duration_since_epoch(timestamp).expiry_time(std::time::Duration::from_secs(3_600))
			.min_final_cltv_expiry_delta(18).amount_milli_satoshis(amount_msat)
			.build_signed(|hash: &Message| secp.sign_ecdsa_recoverable(hash, &key)).unwrap()
	}

	#[test]
	fn strict_lnurl_expired_signed_invoice_refuses_and_current_invoice_passes() {
		let resp = response();
		let expired = signed_invoice_at(&resp.metadata, 10_000_000, false, Currency::BitcoinTestnet,
			std::time::Duration::from_secs(1_700_000_000));
		expired.check_signature().unwrap();
		assert!(expired.is_expired());
		let error = verify_lnurlp_invoice(&expired, Amount::from_sat(10_000), &resp, true).unwrap_err();
		assert!(error.to_string().contains("strict lnurl invoice has expired"));
		verify_lnurlp_invoice(&expired, Amount::from_sat(10_000), &resp, false).unwrap();
		let current = signed_invoice(&resp.metadata, 10_000_000, false);
		current.check_signature().unwrap();
		assert!(!current.is_expired());
		verify_lnurlp_invoice(&current, Amount::from_sat(10_000), &resp, true).unwrap();
	}

	#[test]
	fn strict_lnurl_signed_invoice_binds_exact_raw_metadata_and_amount() {
		let resp = response();
		let invoice = signed_invoice(&resp.metadata, 10_000_000, false);
		invoice.check_signature().unwrap();
		verify_lnurlp_invoice(&invoice, Amount::from_sat(10_000), &resp, true).unwrap();
		let mut changed = resp.clone();
		changed.metadata.push(' ');
		assert!(verify_lnurlp_invoice(&invoice, Amount::from_sat(10_000), &changed, true).is_err());
		assert!(verify_lnurlp_invoice(&invoice, Amount::from_sat(9_000), &resp, true).is_err());
	}

	#[test]
	fn strict_lnurl_refuses_direct_and_wrong_hash_but_keeps_generic_default() {
		let resp = response();
		let direct = signed_invoice(&resp.metadata, 10_000_000, true);
		let wrong = signed_invoice("different advertised bytes", 10_000_000, false);
		for invoice in [direct, wrong] {
			invoice.check_signature().unwrap();
			assert!(verify_lnurlp_invoice(&invoice, Amount::from_sat(10_000), &resp, true).is_err());
			verify_lnurlp_invoice(&invoice, Amount::from_sat(10_000), &resp, false).unwrap();
		}
		assert!(!LightningSendOptions::default().require_lnurl_metadata);
	}

	#[test]
	fn strict_lnurl_recipient_callback_and_transport_contract_refuses_mismatch() {
		let endpoint = "https://wallet.test/.well-known/lnurlp/alice";
		let resp = response();
		verify_strict_lnurl_endpoint(endpoint, &resp, Some("alice@wallet.test")).unwrap();
		assert!(verify_strict_lnurl_endpoint(endpoint, &resp, Some("bob@wallet.test")).is_err());
		for callback in ["http://wallet.test/.well-known/lnurlp/alice", "https://other.test/.well-known/lnurlp/alice", "https://wallet.test/.well-known/lnurlp/bob"] {
			let mut changed = resp.clone(); changed.callback = callback.into();
			assert!(verify_strict_lnurl_endpoint(endpoint, &changed, Some("alice@wallet.test")).is_err());
		}
		for metadata in ["[]", "invalid json", "[[\"text/plain\",\"no identifier\"]]", "[[\"text/identifier\",\"alice@wallet.test\"],[\"text/identifier\",\"alice@wallet.test\"]]"] {
			let mut changed = resp.clone(); changed.metadata = metadata.into();
			assert!(verify_strict_lnurl_endpoint(endpoint, &changed, Some("alice@wallet.test")).is_err());
		}
	}

	#[test]
	fn shared_currency_guard_accepts_signed_tb_for_signet_and_refuses_mainnet() {
		let invoice = signed_invoice(&response().metadata, 10_000_000, false);
		invoice.check_signature().unwrap();
		assert!(bolt11_currency_matches_network(invoice.currency(), Network::Signet));
		assert!(bolt11_currency_matches_network(Currency::Signet, Network::Signet));
		assert!(!bolt11_currency_matches_network(Currency::Bitcoin, Network::Signet));
		assert!(!bolt11_currency_matches_network(invoice.currency(), Network::Bitcoin));
		assert!(!bolt11_currency_matches_network(invoice.currency(), Network::Regtest));
		assert!(bolt11_currency_matches_network(Currency::Regtest, Network::Regtest));
		for currency in [Currency::Simnet, Currency::Bitcoin] {
			let wrong = signed_invoice_for_currency(&response().metadata, 10_000_000, false, currency);
			wrong.check_signature().unwrap();
			assert!(!bolt11_currency_matches_network(wrong.currency(), Network::Regtest));
			assert!(!bolt11_currency_matches_network(wrong.currency(), Network::Signet));
		}
	}
	#[cfg(not(target_arch = "wasm32"))]
	#[test]
	fn strict_lnurl_body_limit_refuses_before_append_and_bad_ca_refuses() {
		let mut body = vec![0; STRICT_LNURL_RESPONSE_LIMIT - 1];
		append_strict_lnurl_chunk(&mut body, &[1]).unwrap();
		let before = body.clone();
		assert!(append_strict_lnurl_chunk(&mut body, &[2]).is_err());
		assert_eq!(body, before);
		let oversized = LightningSendOptions::default().require_lnurl_metadata(true)
			.lnurl_root_ca_pem(vec![0; STRICT_LNURL_RESPONSE_LIMIT + 1]);
		assert!(strict_lnurl_client(&oversized).is_err());
		let malformed = LightningSendOptions::default().require_lnurl_metadata(true)
			.lnurl_root_ca_pem(b"not a certificate".to_vec());
		assert!(strict_lnurl_client(&malformed).is_err());
	}

}
