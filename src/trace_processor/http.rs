//! Thin `reqwest` helpers for sending and decoding protobuf messages over the
//! legacy `trace_processor_shell -D` HTTP endpoints.

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use prost::Message;

const CONTENT_TYPE_PROTO: &str = "application/x-protobuf";

/// POST a prost message and decode the response as another prost message.
pub(crate) async fn post_proto<Req, Resp>(
    client: &reqwest::Client,
    url: &str,
    req: &Req,
) -> Result<Resp>
where
    Req: Message,
    Resp: Message + Default,
{
    let body = req.encode_to_vec();
    post_bytes(client, url, body).await
}

/// POST raw bytes (used by `/parse`, which takes the trace file body directly)
/// and decode the response as a prost message.
pub(crate) async fn post_bytes<Resp>(
    client: &reqwest::Client,
    url: &str,
    body: Vec<u8>,
) -> Result<Resp>
where
    Resp: Message + Default,
{
    let resp = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, CONTENT_TYPE_PROTO)
        .body(body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let tail = resp.text().await.unwrap_or_default();
        bail!("POST {url} -> {status}: {}", tail.trim());
    }
    let bytes: Bytes = resp.bytes().await.with_context(|| format!("body for {url}"))?;
    decode_concat(&bytes)
}

/// GET a prost message.
pub(crate) async fn get_proto<Resp>(
    client: &reqwest::Client,
    url: &str,
) -> Result<Resp>
where
    Resp: Message + Default,
{
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let tail = resp.text().await.unwrap_or_default();
        bail!("GET {url} -> {status}: {}", tail.trim());
    }
    let bytes: Bytes = resp.bytes().await.with_context(|| format!("body for {url}"))?;
    decode_concat(&bytes)
}

/// GET an endpoint where we don't care about the body (e.g. `/notify_eof`).
pub(crate) async fn get_ok(client: &reqwest::Client, url: &str) -> Result<()> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let tail = resp.text().await.unwrap_or_default();
        bail!("GET {url} -> {status}: {}", tail.trim());
    }
    Ok(())
}

/// `/query` splits large result sets into several `QueryResult` messages
/// concatenated on the wire so cells stream as they're produced. Prost's
/// `merge()` reads one message's fields from a slice, but because the proto
/// wire format is field-concatenating — repeated fields append, scalars take
/// the last-seen value — decoding a concatenation of messages of the same
/// type as a single message yields the correct merged view. This works
/// transparently for single-message responses (`/status`, `/parse`) too.
fn decode_concat<M: Message + Default>(bytes: &[u8]) -> Result<M> {
    let mut out = M::default();
    out.merge(bytes).with_context(|| {
        format!(
            "decoding {} bytes as {}",
            bytes.len(),
            std::any::type_name::<M>()
        )
    })?;
    Ok(out)
}
