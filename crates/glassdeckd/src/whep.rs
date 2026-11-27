//! WHEP: a WebRTC page watches a stream with one POST of its SDP offer. Each session sends the frames
//! of one source as RTP, as they come from the encoder, with no decode or re-encode on the way.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, Once};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, ensure};
use glassdeck_proto::Codec;
use str0m::change::SdpOffer;
use str0m::format::Codec as RtpCodec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::sync::mpsc::Receiver;

use crate::feeds::{ViewerFrame, Watching};

const MAX_DATAGRAM: usize = 2000;
const MICROS_PER_90KHZ_TICK_NUMER: u64 = 9;
const MICROS_PER_90KHZ_TICK_DENOM: u64 = 100;

/// A page's offer, with the video codecs it can receive.
pub struct Offer {
    sdp: SdpOffer,
    h264: bool,
    hevc: bool,
}

impl Offer {
    pub fn parse(text: &str) -> Result<Self> {
        let sdp = SdpOffer::from_sdp_string(text).map_err(|e| anyhow!("the offer isn't SDP: {e}"))?;
        let offers = |name: &str| text.lines().any(|line| line.starts_with("a=rtpmap:") && line.to_ascii_uppercase().contains(&format!(" {name}/90000")));
        Ok(Self { sdp, h264: offers("H264"), hevc: offers("H265") })
    }

    fn receives(&self, codec: Codec) -> bool {
        match codec {
            Codec::H264 => self.h264,
            Codec::Hevc => self.hevc,
        }
    }
}

/// Where a viewer's frames come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feed {
    /// The stream's own encoder, the keyboard's when it's playing
    Stream(Codec),
    /// A small H.264 encoder, for a page that can't decode the stream's codec
    Preview,
}

impl Feed {
    /// The stream's own frames when the page can decode them, else the H.264 preview.
    pub fn for_offer(offer: &Offer, stream: Codec) -> Result<Self> {
        if offer.receives(stream) {
            return Ok(Self::Stream(stream));
        }
        ensure!(offer.h264, "the page can't receive H.264 or {stream:?} video");
        Ok(Self::Preview)
    }

    fn codec(self) -> RtpCodec {
        match self {
            Self::Stream(Codec::Hevc) => RtpCodec::H265,
            Self::Stream(Codec::H264) | Self::Preview => RtpCodec::H264,
        }
    }
}

/// The sessions running now, so a DELETE can end one.
#[derive(Default)]
pub struct Sessions {
    next: Mutex<u64>,
    running: Mutex<HashMap<u64, Arc<Notify>>>,
}

impl Sessions {
    /// Answers `offer` and starts sending it what `watch` hands over; returns the session's number and the answer.
    pub async fn start<W: Watching>(self: &Arc<Self>, offer: Offer, feed: Feed, watch: impl FnOnce() -> (W, Receiver<ViewerFrame>)) -> Result<(u64, String)> {
        install_crypto();
        let mut rtc = RtcConfig::new().set_ice_lite(true).clear_codecs().enable_h264(feed.codec() == RtpCodec::H264).enable_h265(feed.codec() == RtpCodec::H265).build(Instant::now());
        let mut sockets = Vec::new();
        for ip in host_addresses() {
            let socket = UdpSocket::bind(SocketAddr::new(ip, 0)).await.with_context(|| format!("binding a UDP port on {ip}"))?;
            rtc.add_local_candidate(Candidate::host(socket.local_addr()?, "udp").context("a host candidate")?);
            sockets.push(socket);
        }
        let answer = rtc.sdp_api().accept_offer(offer.sdp).context("accepting the offer")?.to_sdp_string();

        let id = {
            let mut next = self.next.lock().expect("sessions poisoned");
            *next += 1;
            *next
        };
        let close = Arc::new(Notify::new());
        self.running.lock().expect("sessions poisoned").insert(id, Arc::clone(&close));
        let (viewer, frames) = watch();
        let sessions = Arc::clone(self);
        tokio::spawn(async move {
            let session = Session { rtc, codec: feed.codec(), video: None, viewer };
            if let Err(e) = session.run(&sockets, frames, &close).await {
                eprintln!("WHEP session {id}: {e:#}");
            }
            sessions.running.lock().expect("sessions poisoned").remove(&id);
            println!("WHEP session {id} ended");
        });
        println!("WHEP session {id}: {feed:?}");
        Ok((id, answer))
    }

    /// Whether there was such a session to end.
    pub fn end(&self, id: u64) -> bool {
        let Some(close) = self.running.lock().expect("sessions poisoned").remove(&id) else { return false };
        close.notify_one();
        true
    }
}

struct Session<W> {
    rtc: Rtc,
    codec: RtpCodec,
    /// The page's video m-line and the payload type to send on, once negotiated
    video: Option<(Mid, Pt)>,
    /// Leaves its source when the session ends
    viewer: W,
}

impl<W: Watching> Session<W> {
    async fn run(mut self, sockets: &[UdpSocket], mut frames: Receiver<ViewerFrame>, close: &Notify) -> Result<()> {
        let mut buffers = vec![vec![0u8; MAX_DATAGRAM]; sockets.len()];
        loop {
            let Some(timeout) = self.drive(sockets).await? else { return Ok(()) };
            let (first, rest) = buffers.split_at_mut(1);
            tokio::select! {
                () = close.notified() => {
                    self.rtc.disconnect();
                    return Ok(());
                }
                frame = frames.recv() => match frame {
                    Some(frame) => self.send(frame)?,
                    None => return Ok(()),
                },
                received = receive(&sockets[0], &mut first[0]) => self.receive(received?)?,
                received = receive_maybe(sockets.get(1), rest.first_mut()) => self.receive(received?)?,
                () = tokio::time::sleep_until(timeout.into()) => self.rtc.handle_input(Input::Timeout(Instant::now()))?,
            }
        }
    }

    /// Sends what str0m has to send and handles its events; returns when it next wants waking, or
    /// nothing once the page has gone.
    async fn drive(&mut self, sockets: &[UdpSocket]) -> Result<Option<Instant>> {
        loop {
            if !self.rtc.is_alive() {
                return Ok(None);
            }
            match self.rtc.poll_output()? {
                Output::Timeout(at) => return Ok(Some(at)),
                Output::Transmit(datagram) => {
                    let socket = sockets.iter().find(|s| s.local_addr().is_ok_and(|a| a == datagram.source)).context("no socket for a datagram's source")?;
                    socket.send_to(&datagram.contents, datagram.destination).await?;
                }
                Output::Event(Event::MediaAdded(media)) if media.kind == MediaKind::Video => {
                    let codec = self.codec;
                    let pt = self.rtc.writer(media.mid).and_then(|w| w.payload_params().find(|p| p.spec().codec == codec).map(|p| p.pt()));
                    let pt = pt.with_context(|| format!("the page accepted no {codec} payload"))?;
                    self.video = Some((media.mid, pt));
                    self.viewer.ask_keyframe();
                }
                Output::Event(Event::KeyframeRequest(_)) => self.viewer.ask_keyframe(),
                Output::Event(Event::IceConnectionStateChange(IceConnectionState::Disconnected)) => return Ok(None),
                Output::Event(_) => {}
            }
        }
    }

    fn send(&mut self, frame: ViewerFrame) -> Result<()> {
        let Some((mid, pt)) = self.video else { return Ok(()) };
        let Some(writer) = self.rtc.writer(mid) else { return Ok(()) };
        let ticks = frame.time_us * MICROS_PER_90KHZ_TICK_NUMER / MICROS_PER_90KHZ_TICK_DENOM;
        writer.write(pt, Instant::now(), MediaTime::new(ticks, Frequency::NINETY_KHZ), frame.data)?;
        Ok(())
    }

    fn receive(&mut self, (contents, source, destination): (Vec<u8>, SocketAddr, SocketAddr)) -> Result<()> {
        let Ok(contents) = contents.as_slice().try_into() else { return Ok(()) };
        self.rtc.handle_input(Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination, contents }))?;
        Ok(())
    }
}

/// Windows reports an ICMP "port unreachable" for an earlier send as a reset on the next receive;
/// that's one stale candidate, not the session failing, so it's skipped.
async fn receive(socket: &UdpSocket, buffer: &mut [u8]) -> Result<(Vec<u8>, SocketAddr, SocketAddr)> {
    loop {
        match socket.recv_from(buffer).await {
            Ok((n, source)) => return Ok((buffer[..n].to_vec(), source, socket.local_addr()?)),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

async fn receive_maybe(socket: Option<&UdpSocket>, buffer: Option<&mut Vec<u8>>) -> Result<(Vec<u8>, SocketAddr, SocketAddr)> {
    match (socket, buffer) {
        (Some(socket), Some(buffer)) => receive(socket, buffer).await,
        _ => std::future::pending().await,
    }
}

/// Loopback for pages on this PC, and the address the default route goes out on (Firefox won't use
/// loopback for WebRTC). Only someone with the API token gets a session on either.
fn host_addresses() -> Vec<IpAddr> {
    let mut addresses = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    let routed = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|s| {
        s.connect("192.0.2.1:9")?;
        s.local_addr()
    });
    if let Ok(address) = routed
        && !address.ip().is_unspecified()
        && !address.ip().is_loopback()
    {
        addresses.push(address.ip());
    }
    addresses
}

fn install_crypto() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| str0m::crypto::from_feature_flags().install_process_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(rtpmaps: &[&str]) -> Offer {
        let lines: Vec<String> = rtpmaps.iter().enumerate().map(|(i, codec)| format!("a=rtpmap:{} {codec}/90000", 96 + i)).collect();
        let text = lines.join("\r\n");
        Offer { sdp: SdpOffer::from_sdp_string(TINY_OFFER).unwrap(), h264: text.contains("H264"), hevc: text.contains("H265") }
    }

    const TINY_OFFER: &str = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=mid:0\r\na=ice-ufrag:abcd\r\na=ice-pwd:abcdefghijklmnopqrstuvwx\r\na=fingerprint:sha-256 00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00\r\na=setup:actpass\r\na=recvonly\r\na=rtcp-mux\r\na=rtpmap:96 H264/90000\r\n";

    #[test]
    fn a_page_gets_the_streams_own_frames_when_it_can_decode_them() {
        assert_eq!(Feed::for_offer(&offer(&["H264", "H265"]), Codec::Hevc).unwrap(), Feed::Stream(Codec::Hevc));
        assert_eq!(Feed::for_offer(&offer(&["H264"]), Codec::H264).unwrap(), Feed::Stream(Codec::H264));
    }

    #[test]
    fn a_page_without_hevc_gets_the_h264_preview() {
        assert_eq!(Feed::for_offer(&offer(&["VP8", "H264"]), Codec::Hevc).unwrap(), Feed::Preview);
        assert!(Feed::for_offer(&offer(&["VP8"]), Codec::Hevc).is_err());
    }

    #[test]
    fn the_codecs_come_from_the_offers_rtpmaps() {
        let parsed = Offer::parse(TINY_OFFER).unwrap();
        assert!(parsed.h264 && !parsed.hevc);
    }

    struct Probe;

    impl Watching for Probe {
        fn ask_keyframe(&self) {
            glassdeck_capture::pipeline::request_keyframe();
        }
    }

    struct ToViewers(Arc<Mutex<Vec<tokio::sync::mpsc::Sender<ViewerFrame>>>>);

    impl glassdeck_capture::pipeline::StreamOut for ToViewers {
        fn send(&mut self, message: &glassdeck_proto::Message) -> Result<()> {
            if let glassdeck_proto::Message::Frame(frame) = message {
                let data: Arc<[u8]> = frame.payload.as_slice().into();
                for viewer in self.0.lock().unwrap().iter() {
                    let _ = viewer.try_send(ViewerFrame { time_us: frame.time_us, data: Arc::clone(&data) });
                }
            }
            Ok(())
        }
    }

    /// Serves monitor 1 over WHEP at http://127.0.0.1:8899 for a browser to check by hand (e2e/whep-probe.mjs):
    /// GLASSDECK_PROBE_CODEC=hevc for the stream's codec, else the H.264 preview.
    #[test]
    #[ignore = "needs a monitor, a GPU and a browser; run with cargo test -p glassdeckd probe -- --ignored --nocapture"]
    fn probe_serves_a_monitor_to_a_browser() {
        use glassdeck_capture::sources::{Play, SourceRequest};
        let hevc = std::env::var("GLASSDECK_PROBE_CODEC").is_ok_and(|c| c == "hevc");
        let (codec, feed) = if hevc { (glassdeck_capture::pipeline::VideoCodec::Hevc, Feed::Stream(Codec::Hevc)) } else { (glassdeck_capture::pipeline::VideoCodec::H264, Feed::Preview) };
        let play = Play::Monitor {
            monitor: "1".into(),
            encoder: glassdeck_capture::EncoderKind::Gpu,
            codec,
            mbps: 4,
            fps: 30,
            keyframe_every: 60,
            vbv_frames: None,
            max_width: (!hevc).then_some(960),
        };
        let viewers = Arc::new(Mutex::new(Vec::new()));
        let out = ToViewers(Arc::clone(&viewers));
        std::thread::spawn(move || {
            let source = glassdeck_capture::sources::build(SourceRequest { play, seconds: Some(120) }).unwrap();
            glassdeck_capture::pipeline::run(source, Box::new(out)).unwrap();
        });
        let sessions = Arc::new(Sessions::default());
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let page = axum::routing::get(|| async { axum::response::Html("<video autoplay muted playsinline></video>") });
            let offer = axum::routing::post(move |body: String| {
                let (sessions, viewers) = (Arc::clone(&sessions), Arc::clone(&viewers));
                async move {
                    let offer = Offer::parse(&body).unwrap();
                    let (_, answer) = sessions
                        .start(offer, feed, move || {
                            let (tx, rx) = tokio::sync::mpsc::channel(8);
                            viewers.lock().unwrap().push(tx);
                            (Probe, rx)
                        })
                        .await
                        .unwrap();
                    answer
                }
            });
            let app = axum::Router::new().route("/", page).route("/whep", offer);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:8899").await.unwrap();
            println!("probe ready");
            tokio::time::timeout(std::time::Duration::from_secs(120), axum::serve(listener, app)).await.ok();
        });
    }
}
