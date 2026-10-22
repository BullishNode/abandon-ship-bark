use std::fs::{self, File};

use bitcoin::{bip32::Xpriv, Network};
use tokio_postgres::error::SqlState;

use ark_testing::{Captaind, TestContext};
use bitcoin_ext::rpc::BitcoinRpcExt;
use server::database::Db;
use server::wallet::{read_mnemonic_from_datadir, PersistedWallet, WalletKind};

#[tokio::test]
async fn create_persists_wallets_and_propagates_initial_store_failure() {
	let ctx = TestContext::new("server/create_persists_wallets_and_propagates_initial_store_failure").await;
	let workspace = ark_testing::util::get_cargo_workspace();
	for standalone in [false, true] {
		for reject_store in [false, true] {
			let name = format!("create-{standalone}-{reject_store}");
			let mut cfg = server::Config::load(workspace.join("server/captaind.default.toml")).unwrap();
			cfg.data_dir = ctx.datadir.join(&name);
			cfg.network = Network::Regtest;
			cfg.postgres = ctx.new_postgres(&format!("wallet-create/{name}")).await;
			cfg.bitcoind = server::config::Bitcoind {
				url: ctx.bitcoind().rpc_url(),
				cookie: Some(ctx.bitcoind().rpc_cookie()),
				rpc_user: None,
				rpc_pass: None,
			};
			if reject_store {
				// Install the constraint when migrations create the table. This keeps
				// the database empty for Create and rejects only wallet-state inserts.
				let client = ctx.postgres_manager().database_client(Some(&cfg.postgres.name)).await;
				client.batch_execute("
					CREATE FUNCTION reject_initial_wallet_state() RETURNS event_trigger
					LANGUAGE plpgsql AS $$ BEGIN
						IF EXISTS (SELECT 1 FROM pg_event_trigger_ddl_commands()
							WHERE object_type = 'table' AND object_identity = 'public.wallet_changeset') THEN
							ALTER TABLE wallet_changeset ADD CONSTRAINT reject_initial_wallet_state CHECK (false);
						END IF;
					END $$;
					CREATE EVENT TRIGGER reject_initial_wallet_state ON ddl_command_end
					WHEN TAG IN ('CREATE TABLE') EXECUTE FUNCTION reject_initial_wallet_state();
				").await.unwrap();
			}
			let birthday = ctx.bitcoind().sync_client().deep_tip().unwrap();
			let result = if standalone {
				let mut watchman = server::config::watchmand::Config::load(
					workspace.join("server/watchmand.default.toml"),
				).unwrap();
				watchman.data_dir = cfg.data_dir.clone();
				watchman.network = cfg.network;
				watchman.postgres = cfg.postgres.clone();
				watchman.bitcoind = cfg.bitcoind.clone();
				server::watchman::Daemon::create(watchman).await
			} else {
				let path = ctx.datadir.join(format!("{name}.toml"));
				cfg.write_into(&mut File::create(&path).unwrap()).unwrap();
				let output = Captaind::base_cmd().arg("create").arg("--config").arg(path)
					.output().await.unwrap();
				fs::write(ctx.datadir.join(format!("{name}-stdout.log")), &output.stdout).unwrap();
				fs::write(ctx.datadir.join(format!("{name}-stderr.log")), &output.stderr).unwrap();
				if output.status.success() { Ok(()) } else {
					Err(anyhow::anyhow!("{}", String::from_utf8_lossy(&output.stderr)))
				}
			};
			let db = Db::connect(&cfg.postgres).await.unwrap();
			let count = db.read(async |tx| {
				Ok(tx.query_one("SELECT COUNT(*) FROM wallet_changeset", &[]).await?.get::<_, i64>(0))
			}).await.unwrap();
			if reject_store {
				let error = result.expect_err("Create must fail if the initial wallet state cannot commit");
				assert!(format!("{error:#}").contains("error storing initial wallet state"));
				assert!(format!("{error:#}").contains("reject_initial_wallet_state"));
				if standalone {
					let postgres = error.chain().find_map(|e| e.downcast_ref::<tokio_postgres::Error>())
						.expect("the real database error must propagate").as_db_error().unwrap();
					assert_eq!(postgres.code(), &SqlState::CHECK_VIOLATION);
					assert_eq!(postgres.constraint(), Some("reject_initial_wallet_state"));
				}
				assert_eq!(count, 0, "the rejected initial state must not commit");
				continue;
			}
			result.unwrap();
			assert_eq!(count, if standalone { 1 } else { 2 });
			let rounds = db.read(async |tx| tx.read_aggregate_changeset(WalletKind::Rounds).await).await.unwrap();
			assert_eq!(rounds.is_some(), !standalone);
			let kinds = if standalone { vec![WalletKind::Watchman] } else {
				vec![WalletKind::Rounds, WalletKind::Watchman]
			};
			for kind in &kinds {
				let state = db.read(async |tx| tx.read_aggregate_changeset(*kind).await).await.unwrap()
					.expect("Create must commit each wallet before it returns");
				assert!(state.descriptor.is_some());
				assert_eq!(state.network, Some(Network::Regtest));
				assert_eq!(state.local_chain.blocks.last_key_value(), Some((&birthday.height.to_u32(), &Some(birthday.hash))));
			}
			// A later loader must restore the creation checkpoint, not initialize
			// a fresh wallet at the newer chain tip. No daemon has started.
			ctx.bitcoind().generate(1).await;
			let later = ctx.bitcoind().sync_client().deep_tip().unwrap();
			assert!(later.height > birthday.height);
			let seed = read_mnemonic_from_datadir(&cfg.data_dir).unwrap().to_seed("");
			let master = Xpriv::new_master(Network::Regtest, &seed).unwrap();
			for kind in kinds {
				let wallet = PersistedWallet::load_derive_from_master_xpriv(
					db.clone(), ctx.bitcoind().async_client(), cfg.network, &master,
					kind, later, cfg.min_trusted_confs,
				).await.unwrap();
				assert_eq!(wallet.latest_checkpoint().height(), birthday.height.to_u32());
				assert_eq!(wallet.latest_checkpoint().hash(), birthday.hash);
			}
			let after = db.read(async |tx| {
				Ok(tx.query_one("SELECT COUNT(*) FROM wallet_changeset", &[]).await?.get::<_, i64>(0))
			}).await.unwrap();
			assert_eq!(after, count, "reopen must not insert another initial state");
		}
	}
}
