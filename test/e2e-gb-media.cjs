const puppeteer = require('puppeteer');
const out = process.argv[2] || '/tmp/nes-rtc-test/gb.png';
(async () => {
  const b = await puppeteer.launch({ headless:'new', args:['--no-sandbox','--disable-setuid-sandbox','--autoplay-policy=no-user-gesture-required','--disable-features=WebRtcHideLocalIpsWithMdns'] });
  const p = await b.newPage();
  await p.setViewport({ width: 1100, height: 920, deviceScaleFactor: 1 });
  await p.goto('http://localhost:3000', { waitUntil:'load' });
  await p.click('#connect');
  await p.waitForFunction(async()=>{const pc=window.pc;if(!pc)return false;const r=await pc.getStats();let f=0;r.forEach(s=>{if(s.type==='inbound-rtp'&&s.kind==='video')f=s.framesDecoded||0;});return f>30;},{timeout:20000,polling:500});
  // push through Nintendo + GameFreak logos to the title screen (~6s + Start taps)
  for (let i=0;i<10;i++){ await p.keyboard.press('Enter'); await new Promise(r=>setTimeout(r,500)); }
  await new Promise(r=>setTimeout(r,1500));
  const st = await p.evaluate(async()=>{const r=await window.pc.getStats();let o={};r.forEach(s=>{if(s.type==='inbound-rtp')o[s.kind]={fd:s.framesDecoded||0,w:s.frameWidth||0,h:s.frameHeight||0};});return o;});
  console.log(out.split('/').pop(), JSON.stringify(st));
  await p.screenshot({ path: out });
  await b.close();
})().catch(e=>{console.error('ERR',e);process.exit(2)});
