//! Real, cross-machine media/input transport: an [`webrtc::peer_connection`]
//! (STUN-only ICE via [`crate::ice::IceConfig`], no TURN configured —
//! symmetric-NAT pairs simply won't connect, an accepted limitation) with
//! two [`webrtc::data_channel::DataChannel`]s, `"media"` (host→helper H.264
//! access units) and `"input"` (helper→host input events) — kept separate
//! so a burst of video frames can't head-of-line-block a click.
//!
//! [`WebRtcMediaLink`] implements [`crate::loopback::MediaLink`], the same
//! shape [`crate::loopback::LoopbackTransport`] does, so the streaming
//! pipeline above it (encoder/decoder/pacer/bitrate controller) doesn't
//! need to know which one it's talking to.
//!
//! SDP offer/answer and trickle ICE candidates ride whatever the caller's
//! [`SdpChannel`] already delivers to the peer — in practice,
//! `signaling::SupabaseRealtimeLink`'s `"sdp"` event, on the very same
//! Realtime Broadcast topic the handshake itself used — wrapped through
//! [`crate::signaling::seal`]/[`crate::signaling::open`] with
//! `session_key(SIGNALING_KEY_INFO)`, exactly as that module's own doc
//! comment names as the intended caller.
//!
//! A single data-channel message is capped at ~16KB by the underlying SCTP
//! implementation (see [`webrtc::data_channel::DataChannelEvent::OnMessage`]'s
//! doc comment) — well under a typical encoded video frame — so every
//! message this module sends is chunked and reassembled on the other side.

use crate::ice::IceConfig;
use crate::loopback::{MediaLink, TransportError};
use crate::signaling::{self, SignalingEnvelope};
use bytes::BytesMut;
use consent::{Role, SessionKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent, RTCSessionDescription,
};

const MEDIA_LABEL: &str = "media";
const INPUT_LABEL: &str = "input";
/// Comfortably under the data channel's per-message cap.
const MAX_CHUNK_LEN: usize = 16_000;
const NEGOTIATE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the negotiation loop polls `SdpChannel::try_recv_sdp` — that
/// trait is deliberately synchronous/non-blocking (matching `PeerLink`), so
/// there's no async notification to await instead.
const SDP_POLL_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebRtcError {
    /// Building the `RTCPeerConnection`, creating a data channel, or the
    /// SDP offer/answer/description exchange failed.
    Connect,
    /// No peer showed up (or negotiation stalled) within
    /// [`NEGOTIATE_TIMEOUT`].
    Timeout,
}

impl std::fmt::Display for WebRtcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebRtcError::Connect => write!(f, "WebRTC connection setup failed"),
            WebRtcError::Timeout => write!(f, "WebRTC negotiation timed out"),
        }
    }
}

impl std::error::Error for WebRtcError {}

/// Carries the same HMAC-sealed envelopes [`crate::signaling`] produces,
/// over whatever channel the caller already has open to the peer.
/// Deliberately sync/non-blocking (matching [`consent::transport::
/// PeerLink`]) — `signaling::SupabaseRealtimeLink` implements this on the
/// same joined Realtime channel its `PeerLink` impl uses.
pub trait SdpChannel {
    fn send_sdp(&self, payload: Value);
    fn try_recv_sdp(&self) -> Option<Value>;
}

#[derive(Serialize, Deserialize)]
enum SdpSignal {
    Offer(RTCSessionDescription),
    Answer(RTCSessionDescription),
    IceCandidate(RTCIceCandidateInit),
}

/// JSON-serializable mirror of [`SignalingEnvelope`] — that type's `mac`
/// field stays private outside `crate::signaling` on purpose; this is the
/// one place that needs to put a sealed envelope on the wire.
#[derive(Serialize, Deserialize)]
struct EnvelopeDto {
    payload: Vec<u8>,
    mac: Vec<u8>,
}

fn send_signal(sdp: &impl SdpChannel, key: &SessionKey, signal: &SdpSignal) {
    let Ok(bytes) = serde_json::to_vec(signal) else { return };
    let (payload, mac) = signaling::seal(key, bytes).into_parts();
    let dto = EnvelopeDto { payload, mac: mac.to_vec() };
    if let Ok(value) = serde_json::to_value(&dto) {
        sdp.send_sdp(value);
    }
}

/// `None` covers both "nothing arrived yet" and "arrived but failed to
/// authenticate/parse" — there's no useful way to distinguish a stray
/// message from a hostile one at this layer, and either way it's not a
/// signal to act on.
fn try_recv_signal(sdp: &impl SdpChannel, key: &SessionKey) -> Option<SdpSignal> {
    let value = sdp.try_recv_sdp()?;
    let dto: EnvelopeDto = serde_json::from_value(value).ok()?;
    let mac: [u8; 32] = dto.mac.try_into().ok()?;
    let envelope = SignalingEnvelope::from_parts(dto.payload, mac);
    let payload = signaling::open(key, &envelope).ok()?;
    serde_json::from_slice(&payload).ok()
}

fn to_rtc_ice_servers(ice: &IceConfig) -> Vec<RTCIceServer> {
    ice.servers
        .iter()
        .map(|s| RTCIceServer {
            urls: s.urls.clone(),
            username: s.username.clone().unwrap_or_default(),
            credential: s.credential.clone().unwrap_or_default(),
        })
        .collect()
}

struct Handler {
    ice_tx: tokio_mpsc::UnboundedSender<RTCIceCandidateInit>,
    data_channel_tx: tokio_mpsc::UnboundedSender<Arc<dyn DataChannel>>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(init) = event.candidate.to_json() {
            let _ = self.ice_tx.send(init);
        }
    }

    async fn on_data_channel(&self, data_channel: Arc<dyn DataChannel>) {
        let _ = self.data_channel_tx.send(data_channel);
    }
}

/// Keeps the underlying `RTCPeerConnection` alive and, once the *last*
/// clone drops (shared between a connection's `media` and `input` links —
/// see `negotiate`), tears it down. Splitting this out of
/// [`WebRtcMediaLink`] itself is what lets `negotiate` hand back two plain,
/// freely-movable link values instead of one struct with a `Drop` impl
/// (which Rust would refuse to let callers destructure into its two
/// fields).
struct PcHandle {
    pc: Arc<dyn PeerConnection>,
    runtime: tokio::runtime::Handle,
}

impl Drop for PcHandle {
    fn drop(&mut self) {
        // `Drop` can't be async; hand the actual teardown to the runtime
        // that's been driving this connection all along. Once `pc` closes,
        // the per-channel `poll()` loops spawned in `spawn_link` see
        // `OnClose`/`None` and end themselves.
        let pc = self.pc.clone();
        self.runtime.spawn(async move {
            let _ = pc.close().await;
        });
    }
}

/// One end of a real, DTLS-SRTP-secured WebRTC data channel — see
/// [`crate::loopback::MediaLink`], which this implements.
pub struct WebRtcMediaLink {
    outbound: tokio_mpsc::UnboundedSender<Vec<u8>>,
    inbound: std_mpsc::Receiver<Vec<u8>>,
    disconnected: Arc<AtomicBool>,
    _pc_handle: Arc<PcHandle>,
}

impl MediaLink for WebRtcMediaLink {
    fn send(&mut self, plaintext: &[u8]) {
        let _ = self.outbound.send(plaintext.to_vec());
    }

    fn try_recv(&self) -> Result<Option<Vec<u8>>, TransportError> {
        match self.inbound.try_recv() {
            Ok(bytes) => Ok(Some(bytes)),
            Err(std_mpsc::TryRecvError::Empty) => {
                if self.disconnected.load(Ordering::Relaxed) {
                    Err(TransportError::Disconnected)
                } else {
                    Ok(None)
                }
            }
            Err(std_mpsc::TryRecvError::Disconnected) => Err(TransportError::Disconnected),
        }
    }
}

/// Negotiates a real WebRTC connection with the peer and returns both data
/// channels as `(media, input)`, ready to use. `role == Role::Host` creates
/// the offer (and both data channels, up front, so they ride the initial
/// SDP); `Role::Helper` waits for it and answers, receiving the channels
/// via [`PeerConnectionEventHandler::on_data_channel`]. ICE candidates
/// trickle both ways over `sdp` for as long as either returned link lives —
/// the underlying connection tears down once *both* have dropped (see
/// [`PcHandle`]).
///
/// Must be called from within a tokio runtime (e.g. via `Handle::block_on`
/// from a dedicated OS thread, as `crates/desktop` does) — `webrtc-rs`'s
/// `PeerConnection` is async throughout.
pub async fn negotiate<S>(role: Role, sdp: Arc<S>, key: SessionKey, ice: IceConfig) -> Result<(WebRtcMediaLink, WebRtcMediaLink), WebRtcError>
where
    S: SdpChannel + Send + Sync + 'static,
{
    match tokio::time::timeout(NEGOTIATE_TIMEOUT, negotiate_inner(role, sdp, key, ice)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(WebRtcError::Timeout),
    }
}

async fn negotiate_inner<S>(role: Role, sdp: Arc<S>, key: SessionKey, ice: IceConfig) -> Result<(WebRtcMediaLink, WebRtcMediaLink), WebRtcError>
where
    S: SdpChannel + Send + Sync + 'static,
{
    let (ice_tx, mut ice_rx) = tokio_mpsc::unbounded_channel::<RTCIceCandidateInit>();
    let (dc_tx, mut dc_rx) = tokio_mpsc::unbounded_channel::<Arc<dyn DataChannel>>();
    let handler = Arc::new(Handler { ice_tx, data_channel_tx: dc_tx });

    let configuration = RTCConfigurationBuilder::default().with_ice_servers(to_rtc_ice_servers(&ice)).build();

    let pc: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(configuration)
            .with_handler(handler)
            .with_udp_addrs(vec!["0.0.0.0:0"])
            .build()
            .await
            .map_err(|_| WebRtcError::Connect)?,
    );

    let (media_dc, input_dc) = match role {
        Role::Host => negotiate_as_host(&pc, &*sdp, &key, &mut ice_rx).await?,
        Role::Helper => negotiate_as_helper(&pc, &*sdp, &key, &mut ice_rx, &mut dc_rx).await?,
    };

    // Keeps relaying any further trickled candidates for the life of the
    // session; ends on its own once `pc` closes and `ice_rx` runs dry.
    let runtime = tokio::runtime::Handle::current();
    runtime.spawn(run_ice_pump(pc.clone(), ice_rx, sdp, key));

    let pc_handle = Arc::new(PcHandle { pc, runtime });
    Ok((spawn_link(media_dc, pc_handle.clone()), spawn_link(input_dc, pc_handle)))
}

async fn negotiate_as_host<S: SdpChannel>(
    pc: &Arc<dyn PeerConnection>,
    sdp: &S,
    key: &SessionKey,
    ice_rx: &mut tokio_mpsc::UnboundedReceiver<RTCIceCandidateInit>,
) -> Result<(Arc<dyn DataChannel>, Arc<dyn DataChannel>), WebRtcError> {
    let media_dc = pc.create_data_channel(MEDIA_LABEL, None).await.map_err(|_| WebRtcError::Connect)?;
    let input_dc = pc.create_data_channel(INPUT_LABEL, None).await.map_err(|_| WebRtcError::Connect)?;

    let offer = pc.create_offer(None).await.map_err(|_| WebRtcError::Connect)?;
    pc.set_local_description(offer.clone()).await.map_err(|_| WebRtcError::Connect)?;
    send_signal(sdp, key, &SdpSignal::Offer(offer));

    loop {
        tokio::select! {
            Some(init) = ice_rx.recv() => send_signal(sdp, key, &SdpSignal::IceCandidate(init)),
            _ = tokio::time::sleep(SDP_POLL_INTERVAL) => {
                while let Some(signal) = try_recv_signal(sdp, key) {
                    match signal {
                        SdpSignal::Answer(answer) => {
                            pc.set_remote_description(answer).await.map_err(|_| WebRtcError::Connect)?;
                            return Ok((media_dc, input_dc));
                        }
                        SdpSignal::IceCandidate(init) => {
                            let _ = pc.add_ice_candidate(init).await;
                        }
                        SdpSignal::Offer(_) => {} // a stray retransmit of our own offer — ignore
                    }
                }
            }
        }
    }
}

async fn negotiate_as_helper<S: SdpChannel>(
    pc: &Arc<dyn PeerConnection>,
    sdp: &S,
    key: &SessionKey,
    ice_rx: &mut tokio_mpsc::UnboundedReceiver<RTCIceCandidateInit>,
    dc_rx: &mut tokio_mpsc::UnboundedReceiver<Arc<dyn DataChannel>>,
) -> Result<(Arc<dyn DataChannel>, Arc<dyn DataChannel>), WebRtcError> {
    // Wait for the offer, trickling ICE meanwhile — the host may start
    // gathering (and sending) candidates before we've even seen it.
    let offer = loop {
        tokio::select! {
            Some(init) = ice_rx.recv() => send_signal(sdp, key, &SdpSignal::IceCandidate(init)),
            _ = tokio::time::sleep(SDP_POLL_INTERVAL) => {
                let mut found = None;
                while let Some(signal) = try_recv_signal(sdp, key) {
                    match signal {
                        SdpSignal::Offer(offer) => found = Some(offer),
                        SdpSignal::IceCandidate(init) => { let _ = pc.add_ice_candidate(init).await; }
                        SdpSignal::Answer(_) => {}
                    }
                }
                if let Some(offer) = found {
                    break offer;
                }
            }
        }
    };

    pc.set_remote_description(offer).await.map_err(|_| WebRtcError::Connect)?;
    let answer = pc.create_answer(None).await.map_err(|_| WebRtcError::Connect)?;
    pc.set_local_description(answer.clone()).await.map_err(|_| WebRtcError::Connect)?;
    send_signal(sdp, key, &SdpSignal::Answer(answer));

    // Wait for both data channels the host created, trickling ICE meanwhile.
    let mut media_dc = None;
    let mut input_dc = None;
    while media_dc.is_none() || input_dc.is_none() {
        tokio::select! {
            Some(init) = ice_rx.recv() => send_signal(sdp, key, &SdpSignal::IceCandidate(init)),
            Some(dc) = dc_rx.recv() => {
                match dc.label().await.as_deref() {
                    Ok(MEDIA_LABEL) => media_dc = Some(dc),
                    Ok(INPUT_LABEL) => input_dc = Some(dc),
                    _ => {} // an unrecognized label — not ours, ignore
                }
            }
            _ = tokio::time::sleep(SDP_POLL_INTERVAL) => {
                while let Some(SdpSignal::IceCandidate(init)) = try_recv_signal(sdp, key) {
                    let _ = pc.add_ice_candidate(init).await;
                }
            }
        }
    }

    Ok((media_dc.expect("checked above"), input_dc.expect("checked above")))
}

async fn run_ice_pump<S: SdpChannel>(
    pc: Arc<dyn PeerConnection>,
    mut ice_rx: tokio_mpsc::UnboundedReceiver<RTCIceCandidateInit>,
    sdp: Arc<S>,
    key: SessionKey,
) {
    loop {
        tokio::select! {
            maybe = ice_rx.recv() => {
                match maybe {
                    Some(init) => send_signal(&*sdp, &key, &SdpSignal::IceCandidate(init)),
                    None => return, // the connection (and its handler) is gone
                }
            }
            _ = tokio::time::sleep(SDP_POLL_INTERVAL) => {
                while let Some(signal) = try_recv_signal(&*sdp, &key) {
                    if let SdpSignal::IceCandidate(init) = signal {
                        let _ = pc.add_ice_candidate(init).await;
                    }
                    // a stray/retransmitted Offer or Answer after negotiation
                    // already completed — nothing to do with it.
                }
            }
        }
    }
}

/// Spawns the two background tasks (outbound chunk-and-send, inbound
/// poll-and-reassemble) that back one [`WebRtcMediaLink`].
fn spawn_link(dc: Arc<dyn DataChannel>, pc_handle: Arc<PcHandle>) -> WebRtcMediaLink {
    let (outbound_tx, mut outbound_rx) = tokio_mpsc::unbounded_channel::<Vec<u8>>();
    let (inbound_tx, inbound_rx) = std_mpsc::channel::<Vec<u8>>();
    let disconnected = Arc::new(AtomicBool::new(false));

    {
        let dc = dc.clone();
        tokio::spawn(async move {
            while let Some(bytes) = outbound_rx.recv().await {
                for chunk in chunk_message(&bytes) {
                    if dc.send(chunk).await.is_err() {
                        return;
                    }
                }
            }
        });
    }

    {
        let disconnected = disconnected.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            loop {
                match dc.poll().await {
                    Some(DataChannelEvent::OnMessage(msg)) => {
                        // First byte is the chunk continuation flag (0 =
                        // last chunk of this message, 1 = more follow) —
                        // see `chunk_message`. The channel is ordered, so
                        // simple concatenation reassembles correctly.
                        let Some((&flag, rest)) = msg.data.split_first() else { continue };
                        buf.extend_from_slice(rest);
                        if flag == 0 {
                            let complete = std::mem::take(&mut buf);
                            if inbound_tx.send(complete).is_err() {
                                return; // WebRtcMediaLink dropped
                            }
                        }
                    }
                    Some(DataChannelEvent::OnClose) | None => {
                        disconnected.store(true, Ordering::Relaxed);
                        return;
                    }
                    _ => {} // OnOpen/OnError/buffered-amount events — nothing to do here
                }
            }
        });
    }

    WebRtcMediaLink { outbound: outbound_tx, inbound: inbound_rx, disconnected, _pc_handle: pc_handle }
}

fn chunk_message(bytes: &[u8]) -> Vec<BytesMut> {
    if bytes.is_empty() {
        return vec![BytesMut::from(&[0u8][..])];
    }
    let mut chunks = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let end = (offset + MAX_CHUNK_LEN).min(bytes.len());
        let is_last = end == bytes.len();
        let mut chunk = BytesMut::with_capacity(1 + (end - offset));
        chunk.extend_from_slice(&[if is_last { 0 } else { 1 }]);
        chunk.extend_from_slice(&bytes[offset..end]);
        chunks.push(chunk);
        offset = end;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_message_becomes_a_single_final_chunk() {
        let chunks = chunk_message(b"hello");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0][0], 0);
        assert_eq!(&chunks[0][1..], b"hello");
    }

    #[test]
    fn empty_message_still_produces_one_final_chunk() {
        let chunks = chunk_message(b"");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0][0], 0);
        assert_eq!(chunks[0].len(), 1);
    }

    #[test]
    fn a_message_over_the_chunk_limit_splits_with_only_the_last_marked_final() {
        let big = vec![7u8; MAX_CHUNK_LEN * 2 + 100];
        let chunks = chunk_message(&big);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0][0], 1);
        assert_eq!(chunks[1][0], 1);
        assert_eq!(chunks[2][0], 0);
        let reassembled: Vec<u8> = chunks.iter().flat_map(|c| c[1..].to_vec()).collect();
        assert_eq!(reassembled, big);
    }

    #[test]
    fn envelope_dto_round_trips_a_sealed_signal() {
        use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role as ConsentRole, SessionCode};

        let mut host = HandshakeMachine::new(ConsentRole::Host);
        let mut helper = HandshakeMachine::new(ConsentRole::Helper);
        let code = SessionCode::generate();
        host.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
        host.apply_local(LocalEvent::Confirm).unwrap();
        helper.apply_local(LocalEvent::EnterCode(code.clone())).unwrap();
        helper.apply_local(LocalEvent::Confirm).unwrap();
        host.apply_peer(PeerMessage::Confirm { code: code.clone() }).unwrap();
        helper.apply_peer(PeerMessage::Confirm { code }).unwrap();

        let host_key = host.session_key(crate::signaling::SIGNALING_KEY_INFO).unwrap();
        let helper_key = helper.session_key(crate::signaling::SIGNALING_KEY_INFO).unwrap();

        struct FakeChannel(std::sync::Mutex<Option<Value>>);
        impl SdpChannel for FakeChannel {
            fn send_sdp(&self, payload: Value) {
                *self.0.lock().unwrap() = Some(payload);
            }
            fn try_recv_sdp(&self) -> Option<Value> {
                self.0.lock().unwrap().take()
            }
        }
        let channel = FakeChannel(std::sync::Mutex::new(None));

        let init = RTCIceCandidateInit {
            candidate: "candidate:1 1 udp 1 0.0.0.0 1 typ host".to_string(),
            sdp_mid: Some(String::new()),
            sdp_mline_index: Some(0),
            username_fragment: None,
            url: None,
        };
        send_signal(&channel, &host_key, &SdpSignal::IceCandidate(init.clone()));
        let received = try_recv_signal(&channel, &helper_key).expect("round trip");
        match received {
            SdpSignal::IceCandidate(got) => assert_eq!(got, init),
            _ => panic!("expected an IceCandidate signal"),
        }
    }
}
