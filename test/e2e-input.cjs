// Verify the browser -> data channel -> server controller-input path: connect, then
// press several keys; the server logs each applied "input P1: <kind> <button>".
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
  await new Promise((r) => setTimeout(r, 1000)); // let the data channel open

  const keys = ['ArrowRight', 'x', 'Enter', 'ArrowLeft', 'z'];
  for (const k of keys) {
    await page.keyboard.down(k);
    await new Promise((r) => setTimeout(r, 120));
    await page.keyboard.up(k);
    await new Promise((r) => setTimeout(r, 120));
  }
  await new Promise((r) => setTimeout(r, 500));
  await browser.close();
  console.log('INPUT TEST SENT', keys.length, 'keys');
})().catch((e) => {
  console.error('TEST ERROR', e);
  process.exit(2);
});
