//! One immutable financial route for a fresh server database.

use super::{Db, Tx};

impl Db {
	pub(crate) async fn require_expiry_route(&self, direct: bool) -> anyhow::Result<()> {
		self.write(async |t| t.require_expiry_route(direct).await).await
	}
}

impl Tx<'_> {
	pub(crate) async fn require_expiry_route(&self, direct: bool) -> anyhow::Result<()> {
		// The unique row arbitrates both startup orders and concurrent attempts.
		// Do not update it: a running daemon keeps the route it selected at startup.
		self.execute("INSERT INTO expiry_route (id, direct) VALUES (1,$1)
			ON CONFLICT (id) DO NOTHING", &[&direct]).await?;
		let stored: bool = self.query_one("SELECT direct FROM expiry_route WHERE id=1", &[]).await?.get(0);
		ensure!(stored == direct, "expiry settlement route mismatch; retain the existing state and use matching daemon configurations");
		Ok(())
	}
}
