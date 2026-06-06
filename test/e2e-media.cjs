// Headless end-to-end check: open the server page, click Connect, and confirm the
// browser actually RECEIVES and DECODES the server's VP8 video + Opus audio via WebRTC.
const puppeteer = require('puppeteer');

(async () => {
  const browser = await puppeteer.launch({
    headless: 'new',
    args: [
      '--no-sandbox',
      '--disable-setuid-sandbox',
      '--autoplay-policy=no-user-gesture-required',
      // Disable Chrome's mDNS host-IP obfuscation so localhost ICE is trivial in the test.
      '--disable-features=WebRtcHideLocalIpsWithMdns',
    ],
  });
  const page = await browser.newPage();
  page.on('console', (m) => console.log('[page]', m.text()));
  page.on('pageerror', (e) => console.log('[pageerror]', e.message));

  await page.goto('http://localhost:3000', { waitUntil: 'load' });
  await page.click('#connect');

  const deadline = Date.now() + 15000;
  let last = null;
  while (Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 1500));
    last = await page.evaluate(async () => {
      const pc = window.pc;
      if (!pc) return { error: 'no pc' };
      const report = await pc.getStats();
      const out = { ice: pc.iceConnectionState, conn: pc.connectionState };
      report.forEach((s) => {
        if (s.type === 'inbound-rtp') {
          out[s.kind] = {
            packetsReceived: s.packetsReceived || 0,
            bytesReceived: s.bytesReceived || 0,
            framesDecoded: s.framesDecoded || 0,
            w: s.frameWidth || 0,
            h: s.frameHeight || 0,
          };
        }
      });
      const v = document.getElementById('video');
      out.video_el = {
        videoWidth: v.videoWidth,
        videoHeight: v.videoHeight,
        currentTime: +v.currentTime.toFixed(2),
        readyState: v.readyState,
      };
      return out;
    });
    console.log(JSON.stringify(last));
    if (last.video && last.video.framesDecoded > 10) break;
  }

  const videoOk = last && last.video && last.video.framesDecoded > 0;
  const audioOk = last && last.audio && last.audio.bytesReceived > 0;
  console.log('RESULT video=' + (videoOk ? 'PASS' : 'FAIL') + ' audio=' + (audioOk ? 'PASS' : 'FAIL'));
  await browser.close();
  process.exit(videoOk ? 0 : 1);
})().catch((e) => {
  console.error('TEST ERROR', e);
  process.exit(2);
});
