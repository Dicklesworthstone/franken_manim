//! Recorded-mode adapter over the same listener, parser, authorization policy,
//! bounded client pool and ordinary Studio routes. The only added capability is
//! readonly access to an explicitly supplied, already-validated PCM generation.

use crate::host::*;
use crate::{MAX_AUDIO_CHUNK_FRAMES, StudioRecordedAudio};

impl StudioHost {
    /// Handle one connection with recorded narration enabled.
    ///
    /// All ordinary routes still dispatch through the existing handler. Audio
    /// routes obey the same capability, Host/Origin, expiry, request-size and
    /// rate policy. The source must be admitted by the session asset verifier.
    /// Do not run this concurrently with another accept loop on this host.
    ///
    /// # Errors
    /// Preserves host admission, socket, worker and response errors.
    pub fn serve_recorded_once(&self, audio: &StudioRecordedAudio) -> Result<(), HostError> {
        let (stream, peer) = self.listener.accept()?;
        self.handler.handle_recorded_stream(stream, peer, audio)
    }

    /// Serve the ordinary Studio plus bounded PCM until shutdown, joining all
    /// admitted clients before returning. The supplied immutable recording is
    /// shared, not copied once per request or connection.
    ///
    /// This is an explicit recorded-preview mode. It does not supply sound for
    /// arbitrary live scene programs. A source rejected after reload stops audio
    /// access rather than reusing old narration under a new picture timeline.
    /// The caller remains responsible for shutting down the supervised worker.
    ///
    /// # Errors
    /// As [`Self::serve_until`], plus a recording not admitted by the session.
    pub fn serve_recorded_until(
        &self,
        audio: StudioRecordedAudio,
        shutdown: &AtomicBool,
    ) -> Result<(), HostError> {
        self.handler.admit_recorded_audio(&audio)?;
        let audio = Arc::new(audio);
        let mut clients = client_handle_storage(self.handler.config.max_clients)?;
        self.listener.set_nonblocking(true)?;
        let serve_result = loop {
            if let Err(error) = reap_finished_clients(&mut clients) {
                break Err(error);
            }
            if shutdown.load(Ordering::Acquire) {
                break Ok(());
            }
            match self.listener.accept() {
                Ok((mut stream, peer)) => {
                    let previous = self.active_clients.fetch_add(1, Ordering::AcqRel);
                    if previous >= self.handler.config.max_clients {
                        self.active_clients.fetch_sub(1, Ordering::AcqRel);
                        write_connection_limit_response(&mut stream);
                        continue;
                    }
                    let handler = match self.handler.try_clone() {
                        Ok(handler) => handler,
                        Err(error) => {
                            self.active_clients.fetch_sub(1, Ordering::AcqRel);
                            break Err(error);
                        }
                    };
                    let active = Arc::clone(&self.active_clients);
                    let active_guard = Arc::clone(&active);
                    let audio = Arc::clone(&audio);
                    let client = match std::thread::Builder::new().spawn(move || {
                        let _guard = ActiveClientGuard(active_guard);
                        let _ = handler.handle_recorded_stream(stream, peer, &audio);
                    }) {
                        Ok(client) => client,
                        Err(error) => {
                            active.fetch_sub(1, Ordering::AcqRel);
                            break Err(error.into());
                        }
                    };
                    clients.push(client);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => break Err(error.into()),
            }
        };
        let blocking_result = self
            .listener
            .set_nonblocking(false)
            .map_err(HostError::from);
        self.handler.frames.close();
        let join_result = join_clients(&mut clients);
        serve_result.and(blocking_result).and(join_result)
    }
}

impl HostHandler {
    /// Query the currently admitted worker under the same lock as picture
    /// publication and reload. No soundtrack survives independently in host
    /// storage, so a failed replacement cannot pair new PCM with old pictures.
    pub(in crate::host) fn worker_audio_info(
        &self,
        stream: &mut TcpStream,
    ) -> Result<(), HostError> {
        let body = {
            let _operation = lock(&self.session.operations);
            let mut supervisor = lock(&self.session.supervisor);
            let response = worker_response(supervisor.request(
                SupervisorRequest::AudioInfo {
                    scene: self.session.try_owned_scene()?,
                },
                &*self.session.asset_ok,
            )?)?;
            match response {
                WorkerResponse::AudioInfo {
                    info: Some(info),
                    fps,
                    picture_frames,
                    ..
                } => Some(audio_info_body(
                    info,
                    fps,
                    picture_frames,
                    supervisor.generation(),
                )),
                WorkerResponse::AudioInfo { info: None, .. } => None,
                WorkerResponse::Error { message, .. } => {
                    return Err(HostError::WorkerRejected(message));
                }
                _ => return Err(HostError::UnexpectedWorkerResponse),
            }
        };
        match body {
            Some(body) => write_json_response(stream, body.as_bytes()),
            None => write_http_response(
                stream,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"This capture has no recorded narration; reload after live edits.\n",
            ),
        }
    }

    pub(in crate::host) fn worker_audio_chunk(
        &self,
        stream: &mut TcpStream,
        request: &HttpRequest,
    ) -> Result<(), HostError> {
        let window = audio_window(request)?;
        let (chunk, generation) = {
            let _operation = lock(&self.session.operations);
            let mut supervisor = lock(&self.session.supervisor);
            let generation = supervisor.generation();
            if generation != window.generation {
                return Err(HostError::BadRequest(
                    "stale recorded audio worker generation",
                ));
            }
            let response = worker_response(supervisor.request(
                SupervisorRequest::AudioChunk {
                    scene: self.session.try_owned_scene()?,
                    source: window.source,
                    start: window.start,
                    frames: window.frames,
                },
                &*self.session.asset_ok,
            )?)?;
            let chunk = match response {
                WorkerResponse::AudioChunk { chunk, .. } => chunk,
                WorkerResponse::Error { .. } => {
                    return Err(HostError::BadRequest(
                        "recorded audio source or sample window is no longer available",
                    ));
                }
                _ => return Err(HostError::UnexpectedWorkerResponse),
            };
            let expected = chunk
                .info()
                .sample_frames
                .checked_sub(window.start)
                .map(|n| n.min(u64::from(window.frames)));
            if chunk.info().source != window.source
                || chunk.start() != window.start
                || expected != Some(u64::from(chunk.frames()))
            {
                return Err(HostError::UnexpectedWorkerResponse);
            }
            (chunk, generation)
        };
        write_audio_chunk(stream, &chunk, generation)
    }

    fn admit_recorded_audio(&self, audio: &StudioRecordedAudio) -> Result<(), HostError> {
        if audio.fps == 0 || audio.picture_frames == 0 {
            return Err(HostError::BadRequest(
                "recorded audio needs a nonempty picture clock",
            ));
        }
        if !(self.session.asset_ok)(&audio.source) {
            return Err(HostError::BadRequest(
                "recorded audio source is no longer admitted",
            ));
        }
        Ok(())
    }

    fn handle_recorded_stream(
        &self,
        mut stream: TcpStream,
        peer: SocketAddr,
        audio: &StudioRecordedAudio,
    ) -> Result<(), HostError> {
        if !peer.ip().is_loopback() {
            return Err(HostError::Forbidden("peer is not loopback"));
        }
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(self.config.request_timeout))?;
        stream.set_write_timeout(Some(self.config.request_timeout))?;
        let request = match read_http_request(&mut stream, &self.config) {
            Ok(request) => request,
            Err(
                error @ (HostError::RequestStorageAllocationFailed { .. }
                | HostError::RequestMetadataAllocationFailed { .. }),
            ) => return Err(error),
            Err(error) => {
                let body = error_response_body(&error)?;
                write_http_response(
                    &mut stream,
                    400,
                    "Bad Request",
                    "text/plain; charset=utf-8",
                    body.as_bytes(),
                )?;
                return Ok(());
            }
        };
        let now = self.clock.monotonic();
        if let Err(error) = self.authorize(&request, now) {
            let (status, reason) = match error {
                HostError::Expired => (401, "Unauthorized"),
                HostError::RateLimited => (429, "Too Many Requests"),
                _ => (403, "Forbidden"),
            };
            let body = error_response_body(&error)?;
            return write_http_response(
                &mut stream,
                status,
                reason,
                "text/plain; charset=utf-8",
                body.as_bytes(),
            );
        }
        let result = match (request.method, request.path.as_str()) {
            (Method::Get, "/api/audio") => self.recorded_audio_info(&mut stream, audio),
            (Method::Get, "/api/audio/chunk") => {
                self.recorded_audio_chunk(&mut stream, &request, audio)
            }
            _ => self.route(&mut stream, request, now),
        };
        match result {
            Ok(()) => Ok(()),
            Err(
                error @ (HostError::RequestMetadataAllocationFailed { .. }
                | HostError::ResponseStorageAllocationFailed { .. }),
            ) => Err(error),
            Err(error) => {
                let Some((status, reason)) = error.http_status() else {
                    return Err(error);
                };
                let body = error_response_body(&error)?;
                write_http_response(
                    &mut stream,
                    status,
                    reason,
                    "text/plain; charset=utf-8",
                    body.as_bytes(),
                )
            }
        }
    }

    fn recorded_audio_info(
        &self,
        stream: &mut TcpStream,
        audio: &StudioRecordedAudio,
    ) -> Result<(), HostError> {
        let body = {
            let _operation = lock(&self.session.operations);
            self.admit_recorded_audio(audio)?;
            let generation = lock(&self.session.supervisor).generation();
            audio_info_body(audio.info(), audio.fps, audio.picture_frames, generation)
        };
        write_json_response(stream, body.as_bytes())
    }

    fn recorded_audio_chunk(
        &self,
        stream: &mut TcpStream,
        request: &HttpRequest,
        audio: &StudioRecordedAudio,
    ) -> Result<(), HostError> {
        let window = audio_window(request)?;
        let (chunk, generation) = {
            let _operation = lock(&self.session.operations);
            self.admit_recorded_audio(audio)?;
            let generation = lock(&self.session.supervisor).generation();
            if generation != window.generation || window.source != audio.info().source {
                return Err(HostError::BadRequest(
                    "stale recorded audio source or worker generation",
                ));
            }
            let chunk = audio
                .track
                .chunk(window.source, window.start, window.frames)
                .map_err(|error| match error {
                    crate::StudioAudioError::Allocation => {
                        HostError::Configuration("Studio audio response allocation failed")
                    }
                    _ => HostError::BadRequest("invalid recorded audio sample window"),
                })?;
            (chunk, generation)
        };
        write_audio_chunk(stream, &chunk, generation)
    }
}

struct AudioWindow {
    source: Digest,
    start: u64,
    frames: u32,
    generation: u64,
}

fn audio_window(request: &HttpRequest) -> Result<AudioWindow, HostError> {
    for (key, _) in &request.query {
        if !matches!(
            key.as_str(),
            "cap" | "source" | "start" | "frames" | "worker_generation"
        ) {
            return Err(HostError::BadRequest("unknown recorded audio query field"));
        }
    }
    let field = |name| {
        request
            .query_one(name)?
            .ok_or(HostError::BadRequest("missing recorded audio query field"))
    };
    let source = Digest::from_hex(field("source")?)
        .map_err(|_| HostError::BadRequest("invalid audio source identity"))?;
    let start = field("start")?
        .parse::<u64>()
        .map_err(|_| HostError::BadRequest("invalid audio sample cursor"))?;
    let frames = field("frames")?
        .parse::<u32>()
        .map_err(|_| HostError::BadRequest("invalid audio chunk size"))?;
    if frames == 0 || frames > MAX_AUDIO_CHUNK_FRAMES {
        return Err(HostError::BadRequest("invalid audio chunk size"));
    }
    let generation = field("worker_generation")?
        .parse::<u64>()
        .map_err(|_| HostError::BadRequest("invalid audio worker generation"))?;
    Ok(AudioWindow {
        source,
        start,
        frames,
        generation,
    })
}

fn audio_info_body(
    info: crate::StudioAudioInfo,
    fps: u32,
    picture_frames: u32,
    generation: u64,
) -> String {
    format!(
        "{{\"schema\":\"fmn.studio.audio\",\"version\":1,\"source\":\"{}\",\"sample_rate\":{},\"channels\":{},\"sample_frames\":{},\"fps\":{},\"picture_frames\":{},\"max_chunk_frames\":{},\"worker_generation\":{generation}}}",
        info.source.to_hex(),
        info.sample_rate,
        info.channels,
        info.sample_frames,
        fps,
        picture_frames,
        MAX_AUDIO_CHUNK_FRAMES,
    )
}

fn write_audio_chunk(
    stream: &mut TcpStream,
    chunk: &crate::StudioAudioChunk,
    generation: u64,
) -> Result<(), HostError> {
    write_response_with_headers(
        stream,
        200,
        "OK",
        "application/vnd.frankenmanim.pcm-f32le",
        chunk.bytes(),
        &[
            ("X-FMN-Audio-Source", &chunk.info().source.to_hex()),
            ("X-FMN-Worker-Generation", &generation.to_string()),
            ("X-FMN-Audio-Start", &chunk.start().to_string()),
            ("X-FMN-Audio-Frames", &chunk.frames().to_string()),
            ("X-FMN-Audio-Next", &chunk.next().to_string()),
            (
                "X-FMN-Audio-End",
                if chunk.is_terminal() { "true" } else { "false" },
            ),
            ("X-FMN-SHA256", &chunk.digest().to_hex()),
        ],
    )
}
