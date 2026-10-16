//! Private captured-byte checker. Lifecycle/deadline supervision belongs to
//! the independently admitted caller; this command never opens local state.
use std::io::{Read, Write};

const MAX_REQUEST: usize = 32 * 1024 * 1024;

fn execute() -> anyhow::Result<()> {
	anyhow::ensure!(std::env::args_os().count()==1,"no arguments accepted");
	let mut raw=Vec::new();
	std::io::stdin().lock().take((MAX_REQUEST+1) as u64).read_to_end(&mut raw)?;
	anyhow::ensure!(raw.len()<=MAX_REQUEST,"input exceeds bound");
	let summary=server::native_fee_audit::audit_captured(&raw)?;
	let mut output=serde_json::to_vec(&summary)?;output.push(b'\n');
	let mut out=std::io::stdout().lock();out.write_all(&output)?;out.flush()?;
	Ok(())
}
fn main() {
	// Underlying parser errors/panics must never print original witnesses or
	// caller-controlled SQL strings. The supervisor retains the private input.
	std::panic::set_hook(Box::new(|_|{}));
	if !matches!(std::panic::catch_unwind(execute),Ok(Ok(()))) {
		let _=std::io::stdout().write_all(b"{\"format\":\"c-native-captured-audit-v1\",\"error\":\"captured-audit-refused\",\"complete\":false,\"qualified\":false,\"financial_readiness\":false}\n");
		std::process::exit(1);
	}
}
