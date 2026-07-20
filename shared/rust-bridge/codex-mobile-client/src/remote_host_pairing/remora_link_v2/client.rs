//! Bounded JSON framing for the Remora Link v2 control stream.

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

use super::wire::{ProofV2, RequestV2, ResponseV2};

pub(crate) const MAX_CONTROL_FRAME_BYTES: usize = 65_536;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ControlExchangeError {
    #[error("control frame exceeds the Remora Link v2 limit")]
    FrameTooLarge,
    #[error("control frame is not valid UTF-8 JSON")]
    InvalidJson,
    #[error("control frame violates the Remora Link v2 contract")]
    InvalidMessage,
    #[error("control stream I/O failed")]
    Io,
}

/// Read a request frame and reject both malformed JSON and semantically
/// invalid v2 values before returning it to lifecycle code.
pub(crate) async fn read_request_frame<R>(reader: &mut R) -> Result<RequestV2, ControlExchangeError>
where
    R: AsyncRead + Unpin,
{
    let bytes = read_frame_bytes(reader).await?;
    RequestV2::decode_json(&bytes).map_err(|_| ControlExchangeError::InvalidMessage)
}

/// Read the proof frame with strict unknown-field, encoding, and DER checks.
pub(crate) async fn read_proof_frame<R>(reader: &mut R) -> Result<ProofV2, ControlExchangeError>
where
    R: AsyncRead + Unpin,
{
    let bytes = read_frame_bytes(reader).await?;
    ProofV2::decode_json(&bytes).map_err(|_| ControlExchangeError::InvalidMessage)
}

/// Read a response frame and enforce the generic challenge/terminal
/// invariants. The caller must additionally correlate it to its request.
pub(crate) async fn read_response_frame<R>(
    reader: &mut R,
) -> Result<ResponseV2, ControlExchangeError>
where
    R: AsyncRead + Unpin,
{
    let bytes = read_frame_bytes(reader).await?;
    ResponseV2::decode_json(&bytes).map_err(|_| ControlExchangeError::InvalidMessage)
}

pub(crate) async fn write_request_frame<W>(
    writer: &mut W,
    request: &RequestV2,
) -> Result<(), ControlExchangeError>
where
    W: AsyncWrite + Unpin,
{
    request
        .validate()
        .map_err(|_| ControlExchangeError::InvalidMessage)?;
    write_json_frame(writer, request).await
}

pub(crate) async fn write_proof_frame<W>(
    writer: &mut W,
    proof: &ProofV2,
) -> Result<(), ControlExchangeError>
where
    W: AsyncWrite + Unpin,
{
    proof
        .validate()
        .map_err(|_| ControlExchangeError::InvalidMessage)?;
    write_json_frame(writer, proof).await
}

async fn read_frame_bytes<R>(reader: &mut R) -> Result<Zeroizing<Vec<u8>>, ControlExchangeError>
where
    R: AsyncRead + Unpin,
{
    let length = reader
        .read_u32()
        .await
        .map_err(|_| ControlExchangeError::Io)? as usize;
    if length > MAX_CONTROL_FRAME_BYTES {
        return Err(ControlExchangeError::FrameTooLarge);
    }
    let mut bytes = Zeroizing::new(vec![0_u8; length]);
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|_| ControlExchangeError::Io)?;
    if std::str::from_utf8(&bytes).is_err() {
        return Err(ControlExchangeError::InvalidJson);
    }
    Ok(bytes)
}

/// Serialize and write exactly one bounded UTF-8 JSON control message.
pub(super) async fn write_json_frame<T, W>(
    writer: &mut W,
    value: &T,
) -> Result<(), ControlExchangeError>
where
    T: Serialize,
    W: AsyncWrite + Unpin,
{
    let bytes =
        Zeroizing::new(serde_json::to_vec(value).map_err(|_| ControlExchangeError::InvalidJson)?);
    if bytes.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(ControlExchangeError::FrameTooLarge);
    }
    writer
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|_| ControlExchangeError::Io)?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|_| ControlExchangeError::Io)?;
    writer.flush().await.map_err(|_| ControlExchangeError::Io)
}

#[cfg(test)]
pub(super) async fn read_frame_bytes_for_test<R>(
    reader: &mut R,
) -> Result<Zeroizing<Vec<u8>>, ControlExchangeError>
where
    R: AsyncRead + Unpin,
{
    read_frame_bytes(reader).await
}
