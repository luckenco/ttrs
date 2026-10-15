/** Whole SQLite allocation (including hidden control KV), not logical/live bytes or R2 usage. */
export function storageStatus(sql: SqlStorage, object: string) {
  const bytes = sql.databaseSize, warningBytes = 50_000_000;
  const status = { metric: "sqlite_database_bytes", bytes, warningBytes, warning: bytes >= warningBytes };
  if (status.warning) console.warn(JSON.stringify({ event: "storage_warning", object, ...status }));
  return status;
}
