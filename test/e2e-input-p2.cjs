// Verify Player 2 input: press WASD + J/K and confirm the server logs them as P2.
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
  page.on('console', (m) => console.log('[page]', m.text()));
  await page.goto('http://localhost:3000', { waitUntil: 'load' });
  await page.click('#connect');
  await page.waitForFunction(() => window.pc && window.pc.connectionState === 'connected', {
    timeout: 12000,
  });
  await new Promise((r) => setTimeout(r, 1000));

  // P2: w a s d (D-pad) + j k (B/A). Mixed with a P1 key (x => P1 A) as a control.
  const keys = ['w', 'a', 's', 'd', 'j', 'k', 'x'];
  for (const k of keys) {
    await page.keyboard.down(k);
    await new Promise((r) => setTimeout(r, 110));
    await page.keyboard.up(k);
    await new Promise((r) => setTimeout(r, 110));
  }
  await new Promise((r) => setTimeout(r, 400));
  await browser.close();
  console.log('P2 TEST SENT', keys.join(' '));
})().catch((e) => {
  console.error('TEST ERROR', e);
  process.exit(2);
});
