//! Simple file-based storage for testing, built on top of the memory storage adaptor.

use std::path::{Path, PathBuf};

use tokio::fs;

use crate::persist::adaptor::{Query, QueryRange, Record, StorageAdaptor};

use super::memory::MemoryStorageAdaptor;

#[derive(Debug)]
pub struct FileStorageAdaptor {
	file_path: PathBuf,
}

impl FileStorageAdaptor {
	pub async fn open(file_path: impl AsRef<Path>) -> anyhow::Result<Self> {
		let file_path = file_path.as_ref().to_path_buf();

		crate::fs_perms::warn_if_loose(&file_path, 0o600);

		Ok(Self { file_path })
	}

	pub async fn read(&self) -> anyhow::Result<MemoryStorageAdaptor> {
		let data = match fs::read_to_string(&self.file_path).await {
			Ok(contents) => {
				let records = serde_json::from_str::<Vec<Record>>(&contents)?;

				let mut data = MemoryStorageAdaptor::new();
				for record in records {
					data.put(record).await?;
				}
				data
			}
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => MemoryStorageAdaptor::new(),
			Err(e) => return Err(e.into()),
		};

		Ok(data)
	}

	async fn persist(&self, data: &MemoryStorageAdaptor) -> anyhow::Result<()> {
		let records = data.partitions().values()
			.map(|p| p.values())
			.flatten().collect::<Vec<_>>();
		let json = serde_json::to_vec(&records)?;

		crate::fs_perms::write_atomic_owner_only(&self.file_path, &json)
	}
}

#[async_trait]
impl StorageAdaptor for FileStorageAdaptor {
	async fn put_batch(&mut self, records: Vec<Record>) -> anyhow::Result<()> {
		// This test backend's non-Unix writer truncates in place. Refuse a
		// batch there rather than violating its atomicity contract.
		#[cfg(not(unix))]
		{
			let _ = records;
			bail!("atomic batches require a Unix file store or another storage backend");
		}
		#[cfg(unix)]
		{
			let mut data = self.read().await?;
			data.put_batch(records).await?;
			self.persist(&data).await
		}
	}

	async fn put(&mut self, record: Record) -> anyhow::Result<()> {
		let mut data = self.read().await?;
		data.put(record).await?;
		self.persist(&data).await
	}

	async fn get(&self, partition: u8, pk: &[u8]) -> anyhow::Result<Option<Record>> {
		let data = self.read().await?;
		data.get(partition, pk).await
	}

	async fn delete(&mut self, partition: u8, pk: &[u8]) -> anyhow::Result<Option<Record>> {
		let mut data = self.read().await?;
		let deleted_record = data.delete(partition, pk).await?;
		self.persist(&data).await?;
		Ok(deleted_record)
	}

	async fn query_sorted<R: QueryRange>(&self, query: Query<R>) -> anyhow::Result<Vec<Record>> {
		let data = self.read().await?;
		data.query_sorted(query).await
	}

	async fn get_all(&self, partition: u8) -> anyhow::Result<Vec<Record>> {
		let data = self.read().await?;
		data.get_all(partition).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::persist::adaptor::test_suite;
	use crate::persist::adaptor::StorageAdaptorWrapper;
	use crate::persist::BarkPersister;
	use crate::vtxo::VtxoState;
	use ark::test_util::VTXO_VECTORS;

	#[tokio::test]
	#[cfg(unix)]
	async fn server_spend_survives_reopen() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("wallet.json");
		let db = StorageAdaptorWrapper::new(FileStorageAdaptor::open(&path).await.unwrap());
		crate::persist::test_suite::test_server_spend_records_debit_once(&db).await;
		let history = db.get_all_movements().await.unwrap();
		drop(db);
		let reopened = StorageAdaptorWrapper::new(FileStorageAdaptor::open(path).await.unwrap());
		let id = VTXO_VECTORS.board_vtxo.id();
		assert!(reopened.record_server_spent_vtxo(id, None).await.unwrap().is_none());
		assert_eq!(reopened.get_all_movements().await.unwrap(), history);
		assert_eq!(reopened.get_wallet_vtxo(id).await.unwrap().unwrap().state, VtxoState::Spent);
	}

	#[tokio::test]
	async fn file_adaptor_test_suite() {
		let temp_dir = tempfile::tempdir().unwrap();
		let mut storage = FileStorageAdaptor::open(temp_dir.path().join("test.json")).await.unwrap();
		test_suite::run_all(&mut storage).await;
	}

	#[cfg(unix)]
	#[tokio::test]
	async fn persisted_file_is_owner_only() {
		use std::os::unix::fs::PermissionsExt;

		let temp_dir = tempfile::tempdir().unwrap();
		let path = temp_dir.path().join("wallet.json");
		let mut storage = FileStorageAdaptor::open(&path).await.unwrap();

		storage.put(Record {
			partition: 0,
			pk: b"k".to_vec(),
			sort_key: None,
			data: b"v".to_vec(),
		}).await.unwrap();

		let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
		assert_eq!(mode, 0o600, "wallet state file must be owner-only");
	}
}
