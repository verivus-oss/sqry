//! 4-byte little-endian length-prefix frame codec.
//!
//! Every IPC message (handshake, JSON-RPC request, JSON-RPC response,
//! batch array, shim header) travels as:
//!
//! ```text
//! +--------+------------------------------------+
//! | len:u32|                body                |
//! +--------+------------------------------------+
//! ```
//!
//! - `len` is little-endian, excludes the prefix itself, and must be
//!   `<= MAX_FRAME_BYTES` (64 MiB). Larger frames are rejected at the
//!   codec boundary with [`io::ErrorKind::InvalidData`].
//! - `body` is UTF-8 JSON; one complete JSON document per frame, no
//!   chunking, no streaming.
//! - An EOF at a frame boundary (zero bytes read before the length
//!   prefix) is a clean disconnect and returns `Ok(None)`.
//! - An EOF *inside* a length prefix or body is an
//!   [`io::ErrorKind::UnexpectedEof`] — truncated frames are never
//!   silently swallowed (this is the Phase 8a iter-1 B3 fix).

use std::io;

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Per-frame ceiling. Values above this limit are rejected at
/// [`read_frame`] / [`write_frame`] boundaries. The bound is generous
/// enough for Phase 8b's largest expected MCP tool responses
/// (subgraph exports on monorepo-scale graphs) without forcing the
/// complexity of a chunked protocol.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Errors surfaced by the JSON-flavoured codec helpers in this module.
#[derive(Debug, Error)]
pub enum FrameError {
    /// Low-level transport failure (socket EOF mid-frame, permission,
    /// interrupted syscall, etc.).
    #[error("frame io: {0}")]
    Io(#[from] io::Error),

    /// Frame body failed `serde_json` decode. Used by
    /// [`read_frame_json`] when the caller expects a typed shape.
    #[error("frame json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Write a single framed blob.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] if `body.len() > MAX_FRAME_BYTES`.
/// Propagates write and flush errors from the underlying async writer.
pub async fn write_frame<W>(w: &mut W, body: &[u8]) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    if body.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "frame body length {} exceeds MAX_FRAME_BYTES ({MAX_FRAME_BYTES})",
                body.len()
            ),
        ));
    }
    let len = u32::try_from(body.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame body length exceeds u32::MAX",
        )
    })?;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

/// Read a single framed blob.
///
/// Returns `Ok(None)` when the peer closes cleanly at a frame boundary
/// (zero bytes read before any length-prefix byte). Returns
/// [`io::ErrorKind::UnexpectedEof`] when the stream ends mid-prefix or
/// mid-body.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidData`] when the frame length exceeds
/// [`MAX_FRAME_BYTES`]. Propagates transport read errors from the underlying
/// async reader.
pub async fn read_frame<R>(r: &mut R) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    let mut filled = 0usize;
    while filled < 4 {
        match r.read(&mut len_buf[filled..]).await? {
            0 if filled == 0 => return Ok(None),
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("truncated frame: got {filled}/4 length bytes before EOF"),
                ));
            }
            n => filled += n,
        }
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame len {len} exceeds MAX_FRAME_BYTES ({MAX_FRAME_BYTES})"),
        ));
    }
    let mut body = vec![0u8; len];
    match r.read_exact(&mut body).await {
        Ok(_) => Ok(Some(body)),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("truncated frame body: expected {len} bytes"),
        )),
        Err(e) => Err(e),
    }
}

/// Write a typed value as a framed JSON blob.
///
/// # Errors
///
/// Returns [`FrameError::Json`] when serialization fails and [`FrameError::Io`]
/// when writing the frame fails.
pub async fn write_frame_json<W, T>(w: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let body = serde_json::to_vec(value)?;
    write_frame(w, &body).await?;
    Ok(())
}

/// Read a framed JSON blob and decode into `T`. Returns `Ok(None)` on a
/// clean frame-boundary EOF.
///
/// # Errors
///
/// Returns [`FrameError::Io`] for frame transport failures and
/// [`FrameError::Json`] when the frame body cannot be decoded as `T`.
pub async fn read_frame_json<R, T>(r: &mut R) -> Result<Option<T>, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let Some(bytes) = read_frame(r).await? else {
        return Ok(None);
    };
    let value = serde_json::from_slice(&bytes)?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trip_small_frame() {
        let (mut a, mut b) = duplex(1024);
        let payload = br#"{"hello":"world"}"#;
        write_frame(&mut a, payload).await.expect("write");
        drop(a);
        let got = read_frame(&mut b).await.expect("read").expect("some");
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn rejects_oversize_frame_on_write() {
        let (mut a, _b) = duplex(1024);
        let body = vec![0u8; MAX_FRAME_BYTES + 1];
        let err = write_frame(&mut a, &body)
            .await
            .expect_err("oversize must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn clean_eof_at_frame_boundary_returns_ok_none() {
        let (a, mut b) = duplex(64);
        drop(a); // immediate EOF before any prefix byte
        let got = read_frame(&mut b).await.expect("no error on clean EOF");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn truncated_prefix_is_error() {
        let (mut a, mut b) = duplex(64);
        a.write_all(&[0x01, 0x00]).await.unwrap(); // 2/4 length bytes
        drop(a);
        let err = read_frame(&mut b).await.expect_err("truncated prefix");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(
            err.to_string().contains("got 2/4 length bytes"),
            "got unexpected message: {err}"
        );
    }

    #[tokio::test]
    async fn truncated_body_is_error() {
        let (mut a, mut b) = duplex(64);
        // Claim a 16-byte body, send only 5 bytes.
        let len = 16u32.to_le_bytes();
        a.write_all(&len).await.unwrap();
        a.write_all(b"short").await.unwrap();
        drop(a);
        let err = read_frame(&mut b).await.expect_err("truncated body");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(
            err.to_string().contains("truncated frame body"),
            "got unexpected message: {err}"
        );
    }

    #[tokio::test]
    async fn oversize_read_is_rejected() {
        let (mut a, mut b) = duplex(64);
        let bad_len = (MAX_FRAME_BYTES as u32 + 1).to_le_bytes();
        a.write_all(&bad_len).await.unwrap();
        let err = read_frame(&mut b).await.expect_err("oversize claim");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
