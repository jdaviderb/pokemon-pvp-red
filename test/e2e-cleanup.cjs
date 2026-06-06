// Verify per-peer cleanup: connect, confirm video frames, then close the peer and
// check (server-side) that the writer tasks stop (viewers return to 0).
const puppeteer = require('puppeteer');

(async () => {
  const browser = await puppeteer.launch({
    headless: 'new',
    args: [
      '--no-sandbox',
      '--disable-setuid-sandbox',
      '--autoplay-policy=no-user-gesture-required',
      '--disable-features=WebRtcHideLocalIpsWithMdns',
    ],
  });
  const page = await browser.newPage();
  await page.goto('http://localhost:3000', { waitUntil: 'load' });
  await page.click('#connect');

  // Wait until video is actually decoding.
  await page.waitForFunction(
    async () => {
      const pc = window.pc;
      if (!pc) return false;
      const r = await pc.getStats();
      let frames = 0;
      r.forEach((s) => {
        if (s.type === 'inbound-rtp' && s.kind === 'video') frames = s.framesDecoded || 0;
      });
      return frames > 5;
    },
    { timeout: 12000, polling: 500 }
  );
  console.log('video confirmed decoding; now closing peer');
  await page.evaluate(() => window.pc.close());
  await new Promise((r) => setTimeout(r, 2000));
  await browser.close();
  console.log('CLEANUP TEST DONE');
})().catch((e) => {
  console.error('TEST ERROR', e);
  process.exit(2);
});
