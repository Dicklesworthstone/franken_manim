//! Scene-worker side of the isolated Studio protocol.
//!
//! The worker loop owns no UI or checkpoint storage.  It performs a mandatory
//! exact-version handshake, enforces strictly increasing request ids, delegates
//! scene operations to [`WorkerService`], and turns a scene-code panic into one
//! bounded structured crash report before the process exits.  The supervisor
//! can then replace the entire process; no unwind crosses a process boundary
//! and no dynamic library is patched in place.

use std::any::Any;
use std::collections::TryReserveError;
use std::fmt;
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use fmn_hash::Digest;

use crate::protocol::{
    CURRENT_VERSION, CrashReport, FramingError, ProtocolError, ProtocolLimits, ResponseEnvelope,
    SupervisorRequest, TransportCapabilities, WorkerErrorCode, WorkerResponse, read_request,
    write_response,
};

/// A scene operation was refused without crashing the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceError {
    /// Stable machine-readable class.
    pub code: WorkerErrorCode,
    /// Human diagnostic.
    pub message: String,
}

impl ServiceError {
    /// Construct a typed refusal.
    #[must_use]
    pub fn new(code: WorkerErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for ServiceError {}

/// Scene-specific behavior hosted inside the replaceable worker.
pub trait WorkerService {
    /// Content identity of the executable/service implementation.
    fn build_id(&self) -> Digest;

    /// Frame transports this worker can actually serve.
    fn transports(&self) -> TransportCapabilities {
        TransportCapabilities {
            pipe: true,
            shared_memory: false,
        }
    }

    /// Bind the handshaken supervisor identity and negotiated inline-frame
    /// budget into this worker session.  Implementations can journal the
    /// identity and size render buffers before any scene command runs.
    fn begin_session(
        &mut self,
        _supervisor_build: Digest,
        _max_frame_bytes: usize,
    ) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Execute a handshaken scene request.
    ///
    /// `Hello` and `Shutdown` are consumed by [`serve_worker`] and are never
    /// delegated here.
    fn handle(&mut self, request: SupervisorRequest) -> Result<WorkerResponse, ServiceError>;

    /// Borrow the active scene name for a crash report.
    ///
    /// The protocol driver validates the optional diagnostic before taking
    /// ownership so an invalid scene name cannot suppress the crash report.
    fn active_scene(&self) -> Option<&str> {
        None
    }

    /// Borrow the canonical replay-journal bytes for a crash report.
    ///
    /// The protocol driver copies only the suffix admitted by
    /// [`ProtocolLimits::max_crash_tail_bytes`].
    fn journal_tail(&self) -> &[u8] {
        &[]
    }

    /// Last known state hash for a crash report.
    fn last_state_hash(&self) -> Option<Digest> {
        None
    }
}

/// Why a worker loop ended normally from the transport's perspective.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerServeOutcome {
    /// Supervisor closed the pipe.
    PeerClosed,
    /// Supervisor requested graceful shutdown.
    Shutdown,
    /// Scene code panicked; the report was delivered and the caller should
    /// terminate this worker process with a nonzero status.
    Crashed(CrashReport),
    /// The first request was incompatible or violated the handshake.
    HandshakeRejected,
}

/// The worker loop could not complete its bounded protocol.
#[derive(Debug)]
pub enum WorkerServeError {
    /// Pipe/canonical framing failure.
    Framing(FramingError),
    /// A contained panic's report could not be represented or owned.
    CrashReport(ProtocolError),
    /// A service returned a response reserved to the protocol driver.
    ReservedResponse,
}

impl fmt::Display for WorkerServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Framing(error) => error.fmt(f),
            Self::CrashReport(error) => error.fmt(f),
            Self::ReservedResponse => {
                f.write_str("worker service returned a protocol-driver response")
            }
        }
    }
}

impl std::error::Error for WorkerServeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Framing(error) => Some(error),
            Self::CrashReport(error) => Some(error),
            Self::ReservedResponse => None,
        }
    }
}

impl From<FramingError> for WorkerServeError {
    fn from(error: FramingError) -> Self {
        Self::Framing(error)
    }
}

impl From<ProtocolError> for WorkerServeError {
    fn from(error: ProtocolError) -> Self {
        Self::CrashReport(error)
    }
}

/// Serve one supervisor over a bounded request/response pipe.
///
/// # Errors
///
/// Returns [`WorkerServeError`] when the pipe or canonical protocol fails, a
/// contained panic report cannot be represented or owned, or a service
/// attempts to synthesize a handshake/shutdown response.
pub fn serve_worker(
    service: &mut dyn WorkerService,
    reader: &mut impl Read,
    writer: &mut impl Write,
    limits: ProtocolLimits,
) -> Result<WorkerServeOutcome, WorkerServeError> {
    let hello = match read_request(reader, limits) {
        Ok(request) => request,
        Err(FramingError::Closed) => return Ok(WorkerServeOutcome::PeerClosed),
        Err(error) => return Err(error.into()),
    };
    let SupervisorRequest::Hello {
        version,
        max_frame_bytes,
        supervisor_build,
    } = hello.request
    else {
        write_response(
            writer,
            &ResponseEnvelope {
                request_id: hello.request_id,
                response: worker_error(
                    WorkerErrorCode::InvalidRequest,
                    "Hello must be the first worker request",
                    "worker protocol error",
                    limits,
                ),
            },
            limits,
        )?;
        return Ok(WorkerServeOutcome::HandshakeRejected);
    };
    if hello.request_id == 0 {
        write_response(
            writer,
            &ResponseEnvelope {
                request_id: hello.request_id,
                response: worker_error(
                    WorkerErrorCode::InvalidRequest,
                    "request id zero is reserved",
                    "worker protocol error",
                    limits,
                ),
            },
            limits,
        )?;
        return Ok(WorkerServeOutcome::HandshakeRejected);
    }
    if version.require_current().is_err() {
        write_response(
            writer,
            &ResponseEnvelope {
                request_id: hello.request_id,
                response: worker_error(
                    WorkerErrorCode::VersionSkew,
                    format!(
                        "worker protocol requires {}.{}, peer sent {}.{}",
                        CURRENT_VERSION.major, CURRENT_VERSION.minor, version.major, version.minor
                    ),
                    "worker protocol error",
                    limits,
                ),
            },
            limits,
        )?;
        return Ok(WorkerServeOutcome::HandshakeRejected);
    }
    let peer_frame_budget = match usize::try_from(max_frame_bytes) {
        Ok(budget) => budget,
        Err(_) => {
            write_response(
                writer,
                &ResponseEnvelope {
                    request_id: hello.request_id,
                    response: worker_error(
                        WorkerErrorCode::InvalidRequest,
                        "peer frame budget exceeds this platform",
                        "worker protocol error",
                        limits,
                    ),
                },
                limits,
            )?;
            return Ok(WorkerServeOutcome::HandshakeRejected);
        }
    };
    let mut session_limits = limits;
    session_limits.max_frame_bytes = session_limits.max_frame_bytes.min(peer_frame_budget);
    if let Err(error) = service.begin_session(supervisor_build, session_limits.max_frame_bytes) {
        let ServiceError { code, message } = error;
        write_response(
            writer,
            &ResponseEnvelope {
                request_id: hello.request_id,
                response: worker_error(
                    code,
                    message,
                    "worker service rejected the session",
                    limits,
                ),
            },
            limits,
        )?;
        return Ok(WorkerServeOutcome::HandshakeRejected);
    }
    write_response(
        writer,
        &ResponseEnvelope {
            request_id: hello.request_id,
            response: WorkerResponse::Hello {
                version: CURRENT_VERSION,
                worker_build: service.build_id(),
                transports: service.transports(),
            },
        },
        session_limits,
    )?;

    let mut last_request_id = hello.request_id;
    loop {
        let envelope = match read_request(reader, session_limits) {
            Ok(request) => request,
            Err(FramingError::Closed) => return Ok(WorkerServeOutcome::PeerClosed),
            Err(error) => return Err(error.into()),
        };
        if envelope.request_id <= last_request_id {
            write_response(
                writer,
                &ResponseEnvelope {
                    request_id: envelope.request_id,
                    response: worker_error(
                        WorkerErrorCode::InvalidRequest,
                        "request ids must increase strictly",
                        "worker protocol error",
                        session_limits,
                    ),
                },
                session_limits,
            )?;
            return Ok(WorkerServeOutcome::HandshakeRejected);
        }
        last_request_id = envelope.request_id;

        match envelope.request {
            SupervisorRequest::Hello { .. } => {
                write_response(
                    writer,
                    &ResponseEnvelope {
                        request_id: envelope.request_id,
                        response: worker_error(
                            WorkerErrorCode::InvalidRequest,
                            "Hello may appear only once",
                            "worker protocol error",
                            session_limits,
                        ),
                    },
                    session_limits,
                )?;
                return Ok(WorkerServeOutcome::HandshakeRejected);
            }
            SupervisorRequest::Shutdown => {
                write_response(
                    writer,
                    &ResponseEnvelope {
                        request_id: envelope.request_id,
                        response: WorkerResponse::Bye,
                    },
                    session_limits,
                )?;
                return Ok(WorkerServeOutcome::Shutdown);
            }
            request => {
                let handled = catch_unwind(AssertUnwindSafe(|| service.handle(request)));
                match handled {
                    Ok(Ok(response)) => {
                        let response = match response {
                            WorkerResponse::Error { code, message } => worker_error(
                                code,
                                message,
                                "worker service refused the request",
                                session_limits,
                            ),
                            response => response,
                        };
                        if matches!(response, WorkerResponse::Hello { .. } | WorkerResponse::Bye) {
                            return Err(WorkerServeError::ReservedResponse);
                        }
                        write_response(
                            writer,
                            &ResponseEnvelope {
                                request_id: envelope.request_id,
                                response,
                            },
                            session_limits,
                        )?;
                    }
                    Ok(Err(error)) => {
                        let ServiceError { code, message } = error;
                        write_response(
                            writer,
                            &ResponseEnvelope {
                                request_id: envelope.request_id,
                                response: worker_error(
                                    code,
                                    message,
                                    "worker service refused the request",
                                    session_limits,
                                ),
                            },
                            session_limits,
                        )?;
                    }
                    Err(payload) => {
                        let report = crash_report(service, payload, session_limits)?;
                        let envelope = ResponseEnvelope {
                            request_id: envelope.request_id,
                            response: WorkerResponse::Crash(report),
                        };
                        write_response(writer, &envelope, session_limits)?;
                        let report = match envelope.response {
                            WorkerResponse::Crash(report) => report,
                            _ => return Err(WorkerServeError::ReservedResponse),
                        };
                        return Ok(WorkerServeOutcome::Crashed(report));
                    }
                }
            }
        }
    }
}

fn crash_report(
    service: &dyn WorkerService,
    payload: Box<dyn Any + Send>,
    limits: ProtocolLimits,
) -> Result<CrashReport, ProtocolError> {
    let message = panic_message(
        payload,
        effective_field_limit(limits.max_crash_message_bytes, limits),
    )?;
    let scene = match catch_unwind(AssertUnwindSafe(|| service.active_scene())) {
        Ok(Some(scene)) if !scene.is_empty() && scene.len() <= limits.max_field_bytes => {
            Some(try_clone_string(scene, "crash scene bytes")?)
        }
        Ok(_) | Err(_) => None,
    };
    let tail_limit = effective_field_limit(limits.max_crash_tail_bytes, limits);
    let journal_tail = match catch_unwind(AssertUnwindSafe(|| service.journal_tail())) {
        Ok(tail) => {
            let keep_from = tail.len().saturating_sub(tail_limit);
            let tail = tail
                .get(keep_from..)
                .ok_or(ProtocolError::Malformed("invalid crash journal tail range"))?;
            try_clone_bytes(tail, "crash journal tail bytes")?
        }
        Err(_) => Vec::new(),
    };
    let state_hash =
        catch_unwind(AssertUnwindSafe(|| service.last_state_hash())).unwrap_or_default();
    Ok(CrashReport {
        scene,
        message,
        journal_tail,
        state_hash,
    })
}

fn panic_message(payload: Box<dyn Any + Send>, limit: usize) -> Result<String, ProtocolError> {
    require_crash_message_capacity(limit)?;
    if let Some(message) = payload.downcast_ref::<&str>() {
        return bounded_message(message, limit);
    }
    if let Ok(message) = payload.downcast::<String>() {
        return bounded_owned_crash_message(
            *message,
            "scene worker panicked with an empty message",
            limit,
        );
    }
    bounded_message("scene worker panicked with a non-string payload", limit)
}

fn bounded_owned_crash_message(
    mut message: String,
    fallback: &str,
    limit: usize,
) -> Result<String, ProtocolError> {
    if message.is_empty() {
        return bounded_message(fallback, limit);
    }
    let end = utf8_prefix_boundary(&message, limit);
    if end == 0 && limit > 0 {
        message.clear();
        message
            .try_reserve_exact(1)
            .map_err(|source| storage_unavailable("crash message bytes", 1, source))?;
        message.push('?');
        return Ok(message);
    }
    message.truncate(end);
    Ok(message)
}

fn effective_field_limit(specific_limit: usize, limits: ProtocolLimits) -> usize {
    specific_limit.min(limits.max_field_bytes)
}

fn worker_error(
    code: WorkerErrorCode,
    message: impl Into<String>,
    fallback: &str,
    limits: ProtocolLimits,
) -> WorkerResponse {
    WorkerResponse::Error {
        code,
        message: bounded_owned_error_message(
            message.into(),
            fallback,
            effective_field_limit(limits.max_error_message_bytes, limits),
        ),
    }
}

fn bounded_owned_error_message(mut message: String, fallback: &str, limit: usize) -> String {
    if message.is_empty() {
        return bounded_error_message(fallback, limit);
    }
    let end = utf8_prefix_boundary(&message, limit);
    if end == 0 && limit > 0 {
        return "?".to_owned();
    }
    message.truncate(end);
    message
}

fn bounded_error_message(message: &str, limit: usize) -> String {
    let end = utf8_prefix_boundary(message, limit);
    if end == 0 && limit > 0 {
        "?".to_owned()
    } else {
        message.get(..end).unwrap_or_default().to_owned()
    }
}

fn bounded_message(message: &str, limit: usize) -> Result<String, ProtocolError> {
    require_crash_message_capacity(limit)?;
    let message = if message.is_empty() {
        "scene worker panicked with an empty message"
    } else {
        message
    };
    let end = utf8_prefix_boundary(message, limit);
    if end == 0 && limit > 0 {
        try_clone_string("?", "crash message bytes")
    } else {
        let bounded = message.get(..end).ok_or(ProtocolError::Malformed(
            "invalid crash message UTF-8 boundary",
        ))?;
        try_clone_string(bounded, "crash message bytes")
    }
}

fn require_crash_message_capacity(limit: usize) -> Result<(), ProtocolError> {
    if limit == 0 {
        return Err(ProtocolError::PayloadLimit {
            field: "crash message",
            limit,
            needed: 1,
        });
    }
    Ok(())
}

fn storage_unavailable(
    field: &'static str,
    additional: usize,
    source: TryReserveError,
) -> ProtocolError {
    ProtocolError::StorageUnavailable {
        field,
        additional,
        source,
    }
}

fn try_string_with_capacity(
    additional: usize,
    field: &'static str,
) -> Result<String, ProtocolError> {
    let mut value = String::new();
    value
        .try_reserve_exact(additional)
        .map_err(|source| storage_unavailable(field, additional, source))?;
    Ok(value)
}

fn try_clone_string(source: &str, field: &'static str) -> Result<String, ProtocolError> {
    let mut value = try_string_with_capacity(source.len(), field)?;
    value.push_str(source);
    Ok(value)
}

fn try_vec_with_capacity<T>(
    additional: usize,
    field: &'static str,
) -> Result<Vec<T>, ProtocolError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(additional)
        .map_err(|source| storage_unavailable(field, additional, source))?;
    Ok(values)
}

fn try_clone_bytes(source: &[u8], field: &'static str) -> Result<Vec<u8>, ProtocolError> {
    let mut value = try_vec_with_capacity(source.len(), field)?;
    value.extend_from_slice(source);
    Ok(value)
}

fn utf8_prefix_boundary(value: &str, limit: usize) -> usize {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::{ProtocolError, try_string_with_capacity, try_vec_with_capacity};

    fn assert_storage_refusal(error: &ProtocolError, field: &'static str, additional: usize) {
        assert!(matches!(
            error,
            ProtocolError::StorageUnavailable {
                field: found,
                additional: found_additional,
                ..
            } if *found == field && *found_additional == additional
        ));
        assert!(std::error::Error::source(error).is_some());
    }

    #[test]
    fn crash_report_storage_refusals_are_typed() {
        let message = try_string_with_capacity(usize::MAX, "crash message bytes")
            .expect_err("impossible crash message capacity must refuse");
        assert_storage_refusal(&message, "crash message bytes", usize::MAX);

        let tail = try_vec_with_capacity::<u8>(usize::MAX, "crash journal tail bytes")
            .expect_err("impossible crash tail capacity must refuse");
        assert_storage_refusal(&tail, "crash journal tail bytes", usize::MAX);
    }
}
