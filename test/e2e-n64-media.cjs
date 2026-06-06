const puppeteer = require('puppeteer');
const fs = require('fs');
(async () => {
  const b = await puppeteer.launch({ headless:'new', args:['--no-sandbox','--disable-setuid-sandbox','--autoplay-policy=no-user-gesture-required','--disable-features=WebRtcHideLocalIpsWithMdns'] });
  const p = await b.newPage();
  p.on('console', m=>console.log('[page]', m.text()));
  await p.goto('http://localhost:3000', { waitUntil:'load' });
  await p.click('#connect');
  // wait until video is decoding frames
  await p.waitForFunction(async () => {
    const pc = window.pc; if (!pc) return false;
    const r = await pc.getStats(); let f=0;
    r.forEach(s => { if (s.type==='inbound-rtp' && s.kind==='video') f = s.framesDecoded||0; });
    return f > 40;
  }, { timeout: 20000, polling: 500 });
  // stats
  const stats = await p.evaluate(async () => {
    const r = await window.pc.getStats(); const o = {};
    r.forEach(s => { if (s.type==='inbound-rtp') o[s.kind] = { framesDecoded:s.framesDecoded||0, bytesReceived:s.bytesReceived||0, w:s.frameWidth||0, h:s.frameHeight||0 }; });
    const v = document.getElementById('video');
    o.el = { vw:v.videoWidth, vh:v.videoHeight, ct:+v.currentTime.toFixed(2), rs:v.readyState };
    return o;
  });
  console.log(JSON.stringify(stats));
  // grab a real frame from the <video> into a PNG (visual proof)
  const dataUrl = await p.evaluate(() => {
    const v = document.getElementById('video');
    const c = document.createElement('canvas');
    c.width = v.videoWidth; c.height = v.videoHeight;
    c.getContext('2d').drawImage(v, 0, 0);
    return c.toDataURL('image/png');
  });
  fs.writeFileSync('/tmp/nes-rtc-test/n64-shot.png', Buffer.from(dataUrl.split(',')[1], 'base64'));
  console.log('SHOT saved', stats.video && stats.video.framesDecoded, 'frames decoded');
  await b.close();
})().catch(e=>{console.error('ERR',e);process.exit(2)});
