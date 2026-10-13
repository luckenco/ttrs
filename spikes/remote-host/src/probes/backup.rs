//! Manifest-last R2 publication and a single persisted, retryable alarm job.
use super::Lab;
use crate::sql;
use serde_json::{Value, json};
use wasm_bindgen::JsValue;

impl Lab {
    // One pending job per DO; repeated IDs retain their captured payload and
    // terminal result. This is an SDK probe, not a scalable scheduler/backup format.
    pub(super) async fn job_probe(
        &self,
        probe: &str,
        request: &worker::Request,
    ) -> worker::Result<Value> {
        if probe == "alarm-state" {
            return self.job_state().await;
        }
        let url = request.url()?;
        let id = url
            .query_pairs()
            .find(|(k, _)| k == "job")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_else(|| "job".into());
        if id.is_empty()
            || id.len() > 64
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(worker::Error::RustError("invalid job id".into()));
        }
        let fail = url
            .query_pairs()
            .any(|(k, v)| k == "fail" && v == "before-manifest");
        self.sync(|| {
            let existing = self.sql.rows(
                "SELECT phase FROM lab_jobs WHERE id = ?",
                &[Some(id.clone())],
            )?;
            if existing.is_empty() {
                let payload = self.capture()?.to_string();
                self.sql.rows(
                    "INSERT INTO lab_jobs VALUES (?, ?, 'pending', 0, CAST(? AS INTEGER))",
                    &[
                        Some(id.clone()),
                        Some(payload),
                        Some(u8::from(fail).to_string()),
                    ],
                )?;
            }
            Ok(JsValue::NULL)
        })
        .map_err(worker::Error::from)?;
        // Await persisted alarm before acknowledging, including a resubmission
        // after a crash between the synchronous job commit and setAlarm.
        if !self.pending()?.is_empty() {
            // workers-rs i64 is an offset, not an absolute JS timestamp.
            self.durable.set_alarm(60_000_i64).await?;
        }
        if probe == "backup"
            && let Err(error) = self.complete(&id).await
        {
            self.durable.set_alarm(60_000_i64).await?;
            return Err(error);
        }
        self.job_state().await
    }

    fn pending(&self) -> worker::Result<sql::Rows> {
        self.sql
            .rows("SELECT id FROM lab_jobs WHERE phase = 'pending'", &[])
            .map_err(worker::Error::from)
    }

    async fn job_state(&self) -> worker::Result<Value> {
        Ok(
            json!({"jobs": self.sql.rows("SELECT id, phase, CAST(attempts AS TEXT) FROM lab_jobs ORDER BY id", &[]).map_err(worker::Error::from)?, "completion_receipts": self.sql.rows("SELECT key, outcome FROM lab_receipts WHERE key LIKE 'job:%' ORDER BY key", &[]).map_err(worker::Error::from)?, "alarm": self.durable.get_alarm().await?, "r2_prefix": format!("lab/{}/", self.id)}),
        )
    }

    async fn complete(&self, id: &str) -> worker::Result<()> {
        let rows = self.sql.rows("SELECT payload, phase, CAST(attempts AS TEXT), CAST(fail_once AS TEXT) FROM lab_jobs WHERE id = ?", &[Some(id.into())]).map_err(worker::Error::from)?;
        let Some(row) = rows.first() else {
            return Ok(());
        };
        if row[1].as_deref() == Some("done") {
            return Ok(());
        }
        let payload = row[0].as_ref().expect("non-null payload");
        let fail = row[2].as_deref() == Some("0") && row[3].as_deref() == Some("1");
        self.sql
            .rows(
                "UPDATE lab_jobs SET attempts = attempts + 1 WHERE id = ? AND phase = 'pending'",
                &[Some(id.into())],
            )
            .map_err(worker::Error::from)?;
        let bucket = self.env.bucket("LAB_BACKUPS")?;
        let key = format!("lab/{}/{id}/payload.json", self.id);
        bucket.put(&key, payload.clone()).execute().await?;
        if fail {
            return Err(worker::Error::RustError(
                "injected failure before manifest".into(),
            ));
        }
        // Deterministic keys and captured bytes make retry safe, including a
        // crash after publishing the manifest but before recording completion.
        let manifest =
            json!({"payload": key, "bytes": payload.len(), "job": id, "object_id": self.id})
                .to_string();
        bucket
            .put(format!("lab/{}/{id}/manifest.json", self.id), manifest)
            .execute()
            .await?;
        self.sync(|| {
            self.sql.rows("INSERT OR IGNORE INTO lab_receipts SELECT 'job:' || id, 'success' FROM lab_jobs WHERE id = ? AND phase = 'pending'", &[Some(id.into())])?;
            self.sql.rows("UPDATE lab_jobs SET phase = 'done' WHERE id = ?", &[Some(id.into())])?;
            Ok(JsValue::NULL)
        }).map_err(worker::Error::from)?;
        Ok(())
    }

    pub async fn alarm(&self) -> worker::Result<worker::Response> {
        for row in self.pending()? {
            let id = row[0].as_deref().expect("non-null job id");
            if let Err(error) = self.complete(id).await {
                self.durable.set_alarm(60_000_i64).await?;
                return Err(error);
            }
        }
        let mut state = self.job_state().await?;
        state["runtime_id"] = json!(super::runtime_id());
        worker::Response::from_json(&state)
    }
}
