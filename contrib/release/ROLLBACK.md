# Restore a qualified release

Keep the bitcoin node and PostgreSQL cluster running. Stop the application and
CLN services using the deployment supervisor. Disable duplicate producers and
restarts during recovery. Record the qualified release target, registrations,
all balance fields, confirmed/unconfirmed UTXO sets, source spend fences and
unpaid obligations before any change. Record completed canonical chain views.

Take a supported PostgreSQL backup and a cold copy of server, watchman and CLN
state. The copies contain secrets and stay in private backup storage. They are
not release assets. Use the exact database identity and its original keys.
Keep the original stores intact. Restore into separate paths when possible.
A binary rollback does not itself require a database restore. Do not restore a
snapshot that omits accepted customer commitments.

For PostgreSQL 17 on Ubuntu, use these executable paths with the deployment's
socket, port, database and private backup destination. Use port 5433 explicitly
for a separate cluster beside a default 5432 cluster. Keep credentials in the
original host's protected authentication files:

```sh
/usr/lib/postgresql/17/bin/pg_dump --format=custom --file="$PRIVATE_DUMP" \
  --host="$PGHOST" --port="$PGPORT" --username="$APP_DB_OWNER" \
  --dbname="$PGDATABASE"
/usr/lib/postgresql/17/bin/pg_restore --list "$PRIVATE_DUMP"
```

Repoint the release symlink only to an already qualified compatible release.
Confirm database-schema compatibility before a version rollback. Do not assume
that an earlier executable can read a migrated database. For a same-release
rehearsal, retain that same target and use the fresh backup taken after the
services stopped. If the procedure requires a database restore, use the
matching PostgreSQL 17 executables and a new empty restore database. Preserve
the original database:

```sh
/usr/lib/postgresql/17/bin/createdb --host="$PGHOST" --port="$PGPORT" \
  --username="$PGADMIN" --owner="$APP_DB_OWNER" "$RESTORE_DATABASE"
/usr/lib/postgresql/17/bin/pg_restore --single-transaction --exit-on-error \
  --no-owner --host="$PGHOST" --port="$PGPORT" \
  --username="$APP_DB_OWNER" --dbname="$RESTORE_DATABASE" "$PRIVATE_DUMP"
```

Create the database as its administrator. Run the restore as the intended
database owner, provision the original grants,
and point both private configs at that restored database. Do not run an old
program against a newer schema without a qualified compatibility path.

Start CLN and the application services. Verify installed versions and bundle
checksums, then wait for both completed canonical chain views to reconcile.
Compare all registrations, balance fields, UTXO sets and source spend fences.
The wallet RPC reveals a new receiving address on each call; that field alone
is not a stable balance snapshot. Compare server/watchman seeds and the CLN
HSM secret locally without any display of their contents. Keep absent customer
wallets closed.

Retain the service-port reservations described in the operator manual.
Restore normal supervisor restart behavior only after the identities and
financial state match. Use `Restart=always` for systemd services so that an
unexpected clean exit also restarts the daemon; an administrative stop still
suppresses restart. A listening port and a healthy process alone do not prove
that payout observation resumed. If there is no prior qualified release, keep
the application stopped and retain its data. Never delete financial records or
change an unpaid obligation to make recovery pass. Do not replace current
Lightning channel state with an older cold copy as a routine rollback; follow
the original node's supported channel-recovery procedure.

A same-release rehearsal does not qualify an upgrade or disaster recovery on a
fresh host. Verify customer payout and a seed-restored spend separately.
