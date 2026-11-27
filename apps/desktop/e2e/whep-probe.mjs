// Watches the WHEP probe (glassdeckd's ignored probe test) in Edge and reports what it decoded.
import { chromium } from "@playwright/test";

const hevc = process.argv.includes("--hevc");
const browser = await chromium.launch({
  channel: "msedge",
  headless: !hevc,
  args: hevc ? ["--enable-features=WebRtcAllowH265Receive"] : [],
});
const page = await browser.newPage();
await page.goto("http://127.0.0.1:8899/");
const result = await page.evaluate(async () => {
  const pc = new RTCPeerConnection();
  pc.addTransceiver("video", { direction: "recvonly" });
  const video = document.querySelector("video");
  pc.ontrack = (e) => (video.srcObject = e.streams[0] ?? new MediaStream([e.track]));
  await pc.setLocalDescription(await pc.createOffer());
  const answer = await (await fetch("/whep", { method: "POST", body: pc.localDescription.sdp })).text();
  await pc.setRemoteDescription({ type: "answer", sdp: answer });
  const until = Date.now() + 15000;
  let decoded = 0;
  let codec = "";
  while (Date.now() < until && decoded < 60) {
    await new Promise((r) => setTimeout(r, 250));
    const stats = [...(await pc.getStats()).values()];
    const inbound = stats.find((s) => s.type === "inbound-rtp" && s.kind === "video");
    decoded = inbound?.framesDecoded ?? 0;
    codec = stats.find((s) => s.id === inbound?.codecId)?.mimeType ?? codec;
  }
  return { decoded, codec, size: `${video.videoWidth}x${video.videoHeight}`, ice: pc.iceConnectionState };
});
console.log(JSON.stringify(result));
await browser.close();
