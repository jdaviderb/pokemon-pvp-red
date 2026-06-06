const puppeteer = require('puppeteer');
(async () => {
  const b = await puppeteer.launch({ headless:'new', args:['--no-sandbox','--disable-setuid-sandbox','--autoplay-policy=no-user-gesture-required','--disable-features=WebRtcHideLocalIpsWithMdns'] });
  const p = await b.newPage();
  await p.goto('http://localhost:3000', { waitUntil:'load' });
  await p.click('#connect');
  await p.waitForFunction(() => window.pc && window.pc.connectionState === 'connected', { timeout: 12000 });
  await new Promise(r=>setTimeout(r,1000));
  for (const k of ['o','p']) { await p.keyboard.down(k); await new Promise(r=>setTimeout(r,120)); await p.keyboard.up(k); await new Promise(r=>setTimeout(r,120)); }
  await new Promise(r=>setTimeout(r,400));
  await b.close();
  console.log('SENT o p');
})().catch(e=>{console.error(e);process.exit(2)});
