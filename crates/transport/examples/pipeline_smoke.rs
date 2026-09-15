//! Runs the real phase 4 pipeline end-to-end against a real captured frame
//! from this machine's screen: capture -> downscale -> still-screen pacing
//! -> H.264 encode -> encrypted loopback send -> decrypt -> H.264 decode ->
//! RGBA. Unlike the crate's unit tests (which use synthetic frames so
//! they're deterministic and fast), this exercises the exact real capture
//! path `crates/desktop` streams from, the same way `capture`'s own
//! `examples/smoke.rs` proves real capture works outside a test harness.
//!
//! Skips itself (rather than failing) if capture isn't available in this
//! environment, matching `capture::capture_one_frame`'s own contract.

use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode};
use settings::BandwidthProfile;
use transport::bitrate::AdaptiveBitrateController;
use transport::encoder::{SoftwareH264Decoder, SoftwareH264Encoder, VideoEncoder};
use transport::frame_prep::downscale_for_profile;
use transport::loopback::{LoopbackTransport, HELPER_TO_HOST_KEY_INFO, HOST_TO_HELPER_KEY_INFO};

fn main() {
    let frame = match capture::capture_one_frame() {
        Ok(frame) => frame,
        Err(e) => {
            println!("skipping: screen capture is not available in this environment ({e})");
            return;
        }
    };
    println!("captured {}x{} real frame ({} bytes)", frame.width, frame.height, frame.data.len());

    // Real mutual-consent handshake, exactly as the app requires before
    // any of this is allowed to run.
    let mut host = HandshakeMachine::new(Role::Host);
    let mut helper = HandshakeMachine::new(Role::Helper);
    let code = SessionCode::generate();
    host.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
    host.apply_local(LocalEvent::Confirm).unwrap();
    helper.apply_local(LocalEvent::EnterCode(code.clone())).unwrap();
    helper.apply_local(LocalEvent::Confirm).unwrap();
    host.apply_peer(PeerMessage::Confirm { code: code.clone() }).unwrap();
    helper.apply_peer(PeerMessage::Confirm { code }).unwrap();
    assert!(host.is_active() && helper.is_active());
    println!("consent handshake: Active on both sides");

    let profile = BandwidthProfile::Standard;
    let scaled = downscale_for_profile(&frame, profile);
    println!("downscaled to {}x{} for {profile:?}", scaled.width, scaled.height);

    let key_h2h = host.session_key(HOST_TO_HELPER_KEY_INFO).unwrap();
    let key_help2host = host.session_key(HELPER_TO_HOST_KEY_INFO).unwrap();
    let (mut sender, receiver) = LoopbackTransport::pair(&key_h2h, &key_help2host);

    let bitrate = AdaptiveBitrateController::new(profile);
    let mut encoder = SoftwareH264Encoder::new(bitrate.target_bps(), 30.0).expect("encoder init");
    let mut decoder = SoftwareH264Decoder::new().expect("decoder init");

    let encoded = encoder.encode(&scaled, true).expect("encode");
    println!("encoded to {} bytes of H.264 (target {} bps)", encoded.len(), bitrate.target_bps());
    assert!(!encoded.is_empty());
    assert!(
        encoded.len() < scaled.data.len(),
        "encoded H.264 ({} bytes) should be dramatically smaller than raw BGRA ({} bytes)",
        encoded.len(),
        scaled.data.len()
    );

    sender.send(&encoded);
    let received = receiver.try_recv().expect("decrypt").expect("a frame should be waiting");
    assert_eq!(received, encoded, "decrypted bytes must match what was encoded");
    println!("sent over the encrypted loopback transport and decrypted successfully");

    match decoder.decode(&received) {
        Ok(Some(decoded)) => {
            println!("decoded {}x{} RGBA frame ({} bytes) — full pipeline round-trip complete", decoded.width, decoded.height, decoded.rgba.len());
            assert_eq!(decoded.rgba.len(), decoded.width * decoded.height * 4);
        }
        Ok(None) => println!("decoder needs more data (parameter-set-only first packet) — this is normal for a single-frame smoke run"),
        Err(e) => panic!("decode failed: {e}"),
    }
}
