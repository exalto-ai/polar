# Store migration policy

SQLite `user_version` is the durable store-format boundary. The accepted unversioned schema
is version 0. The current build adopts an empty database or the exact accepted object inventory
as version 7 in one transaction, after running the idempotent current DDL. Unrelated, partial,
or modified version-0 databases fail closed. Versions 1 through 6 belonged to closed preview
branches and are not released upgrade sources.

Future released schema changes must follow this sequence:

1. Add one transactional migration from the previous released version.
2. Test representative data, rollback on failure, and reopening after success.
3. Advance `user_version` only in the same transaction as the schema change.
4. Teach the read-only startup inspector that the exact predecessor is migratable.
5. Retire an older daemon only after that inspector succeeds at the signal boundary.

Missing stores and accepted version-0 stores may initialize or adopt version 7. Exact current
version-7 stores reopen normally. Unknown, abandoned-preview, malformed, and future versions
fail closed before persistent pragmas or schema DDL. Their daemon discovery and database must
remain together until an explicit migration or a user-authorized backup and reset.

This policy makes released upgrades automatic without treating an arbitrary local database as
safe merely because its process is old.
