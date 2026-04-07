use tokio::sync::OnceCell;

use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;
use tracing::{info, warn};

use crate::recording::RecordingOutput;

// ── Cached S3 Client ─────────────────────────────────────────────
//
// `aws_config::load_defaults` performs I/O on every call (reads ~/.aws/,
// queries IMDS, etc.) and adds ~100–500 ms of latency.  Build the client
// once and reuse it for every upload.
static S3_CLIENT: OnceCell<Client> = OnceCell::const_new();

/// Promote project-convention `AWS__*` env vars to the standard `AWS_*` names
/// the SDK reads.  If both are set, the standard `AWS_*` wins (user override).
fn promote_env(project_key: &str, sdk_key: &str) {
    if std::env::var(sdk_key).is_err() {
        if let Ok(val) = std::env::var(project_key) {
            std::env::set_var(sdk_key, val);
        }
    }
}

async fn get_or_init_client() -> Result<&'static Client, String> {
    let client = S3_CLIENT.get_or_init(|| async {
        // Bridge project-convention double-underscore vars (STORAGE__AWS_REGION, etc.)
        // into the standard AWS SDK env vars.  Standard AWS_* takes precedence.
        promote_env("STORAGE__AWS_ACCESS_KEY_ID",     "AWS_ACCESS_KEY_ID");
        promote_env("STORAGE__AWS_SECRET_ACCESS_KEY", "AWS_SECRET_ACCESS_KEY");
        promote_env("STORAGE__AWS_REGION",            "AWS_REGION");
        promote_env("STORAGE__AWS_ENDPOINT_URL_S3",   "AWS_ENDPOINT_URL_S3");
        promote_env("STORAGE__AWS_ENDPOINT_URL",      "AWS_ENDPOINT_URL");

        // Default to "auto" when AWS_REGION is unset — required by Cloudflare R2.
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "auto".to_string());
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_config::Region::new(region))
            .load()
            .await;
        Client::new(&config)
    }).await;
    Ok(client)
}

// ── Content-Type helper ──────────────────────────────────────────

fn audio_content_type(extension: &str) -> &'static str {
    match extension {
        "opus" => "audio/ogg; codecs=opus",
        "wav"  => "audio/wav",
        other => {
            warn!(
                "[s3_store] Unknown audio extension {:?} — using application/octet-stream",
                other
            );
            "application/octet-stream"
        }
    }
}

// ── Public API ───────────────────────────────────────────────────

/// Uploads the recording output to an S3/R2 bucket asynchronously.
/// Supports standard AWS env vars: AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY,
/// AWS_REGION, AWS_ENDPOINT_URL_S3.
pub async fn upload_async(output: &RecordingOutput, output_uri: &str) -> Result<String, String> {
    // Parse output_uri like "s3://my-bucket/prefix"
    let without_scheme = output_uri
        .strip_prefix("s3://")
        .ok_or_else(|| "URI must start with s3://".to_string())?;

    // Split into bucket and path prefix
    let (bucket, path_prefix) = match without_scheme.split_once('/') {
        Some((b, p)) => (b, p.trim_end_matches('/')),
        None => (without_scheme, ""),
    };

    let object_key = if path_prefix.is_empty() {
        format!("{}.{}", output.session_id, output.audio_extension)
    } else {
        format!("{}/{}.{}", path_prefix, output.session_id, output.audio_extension)
    };

    let client = get_or_init_client().await?;

    let content_type = audio_content_type(output.audio_extension);

    info!(
        "[s3_store] Uploading audio to s3://{}/{} ({} bytes)",
        bucket, object_key, output.audio_bytes.len()
    );

    // upload_async borrows RecordingOutput, so a clone is unavoidable here.
    // Refactor RecordingOutput to hold Bytes internally to eliminate this copy.
    client
        .put_object()
        .bucket(bucket)
        .key(&object_key)
        .body(ByteStream::from(Bytes::from(output.audio_bytes.clone())))
        .content_type(content_type)
        .send()
        .await
        .map_err(|e| format!("S3 put_object (audio) failed: {}", e))?;

    // Upload transcript if present
    if let Some(ref transcript_bytes) = output.transcript_json {
        let tx_key = if path_prefix.is_empty() {
            format!("{}.json", output.session_id)
        } else {
            format!("{}/{}.json", path_prefix, output.session_id)
        };

        info!("[s3_store] Uploading transcript to s3://{}/{}", bucket, tx_key);

        if let Err(e) = client
            .put_object()
            .bucket(bucket)
            .key(&tx_key)
            .body(ByteStream::from(Bytes::from(transcript_bytes.clone())))
            .content_type("application/json")
            .send()
            .await
        {
            warn!("[s3_store] Failed to upload transcript: {} — audio is still available at s3://{}/{}", e, bucket, object_key);
        }
    }

    // Return the canonical S3 URI so it gets stored in the DB
    Ok(format!("s3://{}/{}", bucket, object_key))
}
