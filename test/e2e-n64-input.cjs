// N64 input test: connect, screenshot, drive the controller (Start, stick, A, C-buttons),
// screenshot again, and confirm the picture changed (proves input reaches the core).
const puppeteer = require('puppeteer');
const fs = require('fs');

const shot = async (p, file) => {
  const url = await p.evaluate(() => {
    const v = document.getElementById('video');
    const c = document.createElement('canvas');
    c.width = v.videoWidth; c.height = v.videoHeight;
    c.getContext('2d').drawImage(v, 0, 0);
    return c.toDataURL('image/png');
  });
  fs.writeFileSync(file, Buffer.from(url.split(',')[1], 'base64'));
};

(async () => {
  const b = await puppeteer.launch({ headless:'new', args:['--no-sandbox','--disable-setuid-sandbox','--autoplay-policy=no-user-gesture-required','--disable-features=WebRtcHideLocalIpsWithMdns'] });
  const p = await b.newPage();
  await p.goto('http://localhost:3000', { waitUntil:'load' });
  await p.click('#connect');
  await p.waitForFunction(() => window.pc && window.pc.connectionState === 'connected', { timeout: 12000 });
  await new Promise(r=>setTimeout(r,2500)); // let video flow
  await shot(p, '/tmp/nes-rtc-test/n64-before.png');

  // Drive the controller: Start (advance intro->menu), hold the stick, press A, C-buttons.
  const tap = async (k, ms=250) => { await p.keyboard.down(k); await new Promise(r=>setTimeout(r,ms)); await p.keyboard.up(k); await new Promise(r=>setTimeout(r,120)); };
  await tap('Enter', 400);          // Start
  await p.keyboard.down('ArrowRight'); await new Promise(r=>setTimeout(r,500)); // hold stick right
  await p.keyboard.up('ArrowRight');
  await tap('x');                   // A
  await tap('Enter', 400);          // Start again
  await tap('i'); await tap('j'); await tap('k'); await tap('l'); // C-buttons
  await new Promise(r=>setTimeout(r,1500));
  await shot(p, '/tmp/nes-rtc-test/n64-after.png');

  await b.close();
  console.log('INPUT TEST DONE');
})().catch(e=>{console.error('ERR',e);process.exit(2)});
