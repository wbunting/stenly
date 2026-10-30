import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { chromium } from '@playwright/test';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const origin = 'http://127.0.0.1:1112';
const REPO = 'https://github.com/wbunting/stenly';
const MAC_DMG = `${REPO}/releases/latest/download/stenly-macos.dmg`;
const LINUX_TARBALL = 'stenly-linux.tar.gz';
const RELEASE_NOTES = `${REPO}/releases/latest`;
// Pricing is decided (Linear DEV-23): the source is MIT and free to build; the signed,
// notarized macOS app is $10 one-time for one Mac, updates included, after a free trial.
// Anything beyond that stays unclaimed.
const PRICE = '$10';
const FREE_MEETINGS = 10;
const UNDECIDED_CLAIM = /€\s?\d|£\s?\d|\$\s?(?!10\b)\d|free forever|money-back|refund|lifetime|subscription|per month|\/month|\bpro\b|unlimited|team plan/i;
const zola = process.env.ZOLA_BIN || 'zola';
let browser, server;
before(async () => {
  const css = spawnSync('npm', ['run','css'], {stdio:'inherit'});
  assert.equal(css.status, 0);
  server = spawn(zola, ['serve','--interface','127.0.0.1','--port','1112'], {stdio:['ignore','pipe','pipe']});
  await new Promise((resolve,reject) => {
    const timer = setTimeout(() => reject(new Error('Zola did not become ready within 10 seconds')),10000);
    const output = chunk => { if (chunk.toString().includes('Web server is available')) {clearTimeout(timer);resolve();} };
    server.stdout.on('data',output);server.stderr.on('data',output);
    server.on('error',e=>{clearTimeout(timer);reject(e)});
    server.on('exit',code=>{clearTimeout(timer);reject(new Error(`Zola exited: ${code}`))});
  });
  browser = await chromium.launch(process.env.PLAYWRIGHT_CHANNEL ? {channel:process.env.PLAYWRIGHT_CHANNEL} : {});
});
after(async () => { await browser?.close(); if(server && server.exitCode === null){server.kill();await once(server,'exit');} });

for (const [width,height] of [[1440,900],[1280,800],[1024,768],[768,1024],[390,844],[320,640]]) {
 test(`responsive page ${width}×${height}`,async()=>{
  const page=await browser.newPage({viewport:{width,height},userAgent:'Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0.0.0 Safari/537.36'});
  await page.addInitScript(() => { Object.defineProperty(navigator, 'platform', {value:'Win32'}); Object.defineProperty(navigator, 'userAgentData', {value:undefined}); });
  const errors=[];page.on('pageerror',e=>errors.push(e.message));page.on('response',r=>{if(r.status()>=400)errors.push(`${r.status()} ${r.url()}`)});
  await page.goto(origin);await page.evaluate(()=>document.fonts.ready);
  const size=await page.evaluate(()=>({w:innerWidth,h:innerHeight,sw:document.documentElement.scrollWidth,sh:document.documentElement.scrollHeight}));
  assert.ok(size.sw<=width,JSON.stringify(size));
  if(width>=1280)assert.ok(size.sh<=height,JSON.stringify(size));
  assert.deepEqual(errors,[]);
  assert.equal(await page.locator('h1').count(),1);
  assert.equal(await page.locator('.source-link').innerText(), 'Source on GitHub');
  assert.equal(await page.locator('.nav-link').filter({hasText:'Availability'}).count(),1);
  assert.equal(await page.locator('.source-link').evaluate(el=>el.tagName), 'A');
  assert.equal(await page.locator('.source-link').getAttribute('href'), REPO);
  assert.equal(await page.locator('#download-macos').getAttribute('href'), MAC_DMG);
  assert.match(await page.locator('#download-macos').innerText(),/Download for macOS/);
  assert.equal(await page.locator('.download-picker').count(),0);
  assert.equal(await page.locator('.platform-link').count(),0);
  await page.getByRole('link',{name:'Pricing',exact:true}).click();
  assert.match(page.url(),/\/pricing\/?$/);
  await page.goto(origin);
  await page.getByRole('button',{name:'Transcript',exact:true}).click();
  assert.equal(await page.locator('#transcript-panel').isVisible(),true);
  assert.equal(await page.locator('#notes-panel').isVisible(),false);
  await page.getByRole('button',{name:'Notes',exact:true}).click();
  assert.equal(await page.locator('#notes-panel').isVisible(),true);
  await page.close();
 });
}
test('main navigation and example switcher work',async()=>{
 const page=await browser.newPage();
 await page.goto(origin);
 await page.getByRole('link',{name:'Pricing',exact:true}).click();
 assert.match(page.url(),/\/pricing\/?$/);
 await page.goto(origin);
 await page.getByRole('link',{name:'Availability',exact:true}).click();
 assert.match(page.url(),/#get-stenly$/);
 await page.getByRole('button',{name:'Transcript',exact:true}).click();
 assert.equal(await page.locator('#transcript-panel').isVisible(),true);
 await page.getByRole('button',{name:'Notes',exact:true}).click();
 assert.equal(await page.locator('#notes-panel').isVisible(),true);
 await page.close();
});
test('macOS download is live and Linux is coming soon, without JavaScript',async()=>{
 const context=await browser.newContext({javaScriptEnabled:false});const page=await context.newPage();await page.goto(origin);
 assert.equal(await page.locator('.source-link').getAttribute('href'),REPO);
 assert.equal(await page.locator('#download-macos').getAttribute('href'),MAC_DMG);
 assert.equal(await page.locator('.download-picker').count(),0);
 assert.equal(await page.locator('[data-platform]').count(),0);
 assert.equal(await page.locator('#downloads-unavailable').count(),0);
 const details=await page.locator('#download-details').innerText();
 assert.match(details,/macOS 14\.2 or later/);
 assert.match(details,/Apple Silicon and Intel/);
 assert.equal(await page.locator('#download-details').getByRole('link',{name:'Release notes'}).getAttribute('href'),RELEASE_NOTES);
 assert.match(await page.locator('#linux-status').innerText(),/Linux: coming soon/);
 const guides=await page.locator('#linux-status a').evaluateAll(links=>links.map(a=>a.getAttribute('href')));
 assert.deepEqual(guides,['/install/ubuntu/','/install/fedora/','/install/arch/']);
 assert.match(await page.locator('.feature-strip').innerText(),/Encrypted sharing is available/);
 assert.equal(await page.getByRole('link',{name:'share.stenly.ai'}).getAttribute('href'),'https://share.stenly.ai');
 assert.match(await page.locator('.feature-strip').innerText(),/your configured server/);
 assert.match(await page.locator('.feature-strip').innerText(),/Self-hosted company mode works/);
 assert.match(await page.locator('.feature-strip').innerText(),/hosted company sign-in is unverified/i);
 assert.equal(await page.locator('#notes-panel').isVisible(),true);
 assert.equal(await page.locator('.note-tabs').isVisible(),false);
 await context.close();
});
test('Linux installation pages are published',async()=>{
 const page=await browser.newPage();await page.goto(origin);
 for (const [path,title] of [
  ['/install/linux/','Install Stenly on Linux'],
  ['/install/ubuntu/','Install Stenly on Ubuntu / Debian'],
  ['/install/fedora/','Install Stenly on Fedora'],
  ['/install/arch/','Install Stenly on Arch Linux'],
 ]) {
  const response=await page.goto(`${origin}${path}`);assert.equal(response.status(),200,path);
  assert.equal(await page.locator('h1').count(),1,path);
  assert.equal(await page.title(),`${title} — Stenly`,path);
  assert.ok((await page.locator('meta[name="description"]').getAttribute('content')).startsWith(`${title}.`),path);
  assert.equal(await page.locator('link[rel="canonical"]').getAttribute('href'),`https://stenly.ai${path}`,path);
  const notice=await page.locator('.install-availability').innerText();
  assert.match(notice,/Linux installers are not available yet/);
  assert.match(notice,/pre-release/);
  assert.match(notice,/cannot complete a Linux installation from them today/);
  assert.doesNotMatch(notice,/Public desktop installers are not available/);
  assert.equal(await page.locator('.install-availability a').getAttribute('href'),'/#get-stenly');
  assert.doesNotMatch(await page.locator('main').innerHTML(),new RegExp(LINUX_TARBALL.replace(/\./g,'\\.')),path);
  if(path==='/install/linux/'){
   const guide=await page.locator('main').innerText();
   assert.match(guide,/Public Linux installers are not available yet/);
   assert.doesNotMatch(guide,/curl -L .*stenly-linux\.tar\.gz/);
  }
 }
 await page.close();
});
test('reduced motion and local assets',async()=>{
 const page=await browser.newPage({reducedMotion:'reduce'});const urls=[];page.on('request',r=>urls.push(r.url()));await page.goto(origin);await page.evaluate(()=>document.fonts.ready);
 assert.equal(await page.locator('.waveform i').first().evaluate(e=>getComputedStyle(e).animationName),'none');
 assert.ok(urls.every(url=>url.startsWith(origin)),JSON.stringify(urls));await page.close();
});
test('Pages build keeps assets same-origin and canonical URLs production',async()=>{
 const temp=await mkdtemp(join(tmpdir(),'stenly-pages-'));
 try {
  const output=join(temp,'output');
  const result=spawnSync(zola,['build','--output-dir',output],{encoding:'utf8'});
  assert.equal(result.status,0,result.stdout+result.stderr);
  const html=await readFile(join(output,'index.html'),'utf8');
  assert.match(html,/Source on GitHub/);
  assert.ok(html.includes(`href=${MAC_DMG}`),'homepage must link the latest-release macOS DMG');
  assert.ok(html.includes(`href=${RELEASE_NOTES}`),'homepage must link the release notes');
  assert.match(html,/Linux: coming soon/);
  assert.doesNotMatch(html,/not available yet|Public source pending|downloads-unavailable/);
  assert.match(html,/Encrypted sharing is available/);
  assert.match(html,/your configured server/);
  assert.match(html,/href=https:\/\/share\.stenly\.ai/);
  assert.doesNotMatch(html,/Get it for Linux|build-from-source/);
  assert.match(html,/href=https:\/\/stenly\.ai\/ rel=canonical/);
  assert.match(html,/content=https:\/\/stenly\.ai\/ property=og:url/);
  assert.equal((html.match(/<a class=source-link href=https:\/\/github\.com\/wbunting\/stenly>/g)||[]).length,1);
  assert.equal((html.match(/class=nav-link href=\/#get-stenly/g)||[]).length,1);
  for (const asset of ['/site.css','/site.js','/fonts/dm-sans.woff2','/fonts/instrument-serif.woff2','/favicon.svg','/icons/apple.svg','/icons/linux.svg','href=/pricing/','href=/#get-stenly','share.stenly.ai']) assert.ok(html.includes(asset),`missing same-origin asset/link: ${asset}`);
  assert.doesNotMatch(html,/https:\/\/stenly\.ai\/(site\.css|site\.js|fonts\/|favicon\.svg|icons\/)/);
  assert.doesNotMatch(html,/https:\/\/stenly\.ai\/(pricing|install)\//,"homepage internal links must stay relative to the preview host");
  const githubLinks=[...html.matchAll(/href=["']?(https:\/\/github\.com\/wbunting\/stenly[^\s"'>]*)/g)].map(m=>m[1]);
  assert.deepEqual([...new Set(githubLinks)].sort(),[REPO,RELEASE_NOTES,MAC_DMG].sort(),'homepage may link only the repo, release notes and macOS DMG');
  const pricing=await readFile(join(output,'pricing','index.html'),'utf8');
  assert.match(pricing,/href=https:\/\/stenly\.ai\/pricing\/ rel=canonical/);
  assert.ok(pricing.includes(`href=${MAC_DMG}`),'pricing must link the macOS DMG');
  assert.ok(pricing.includes(`href=${REPO}`),'pricing must link the repository');
  assert.doesNotMatch(pricing,/not available yet|Desktop release pending|Pricing to be announced/);
  assert.match(pricing,/content=https:\/\/stenly\.ai\/pricing\/ property=og:url/);
  for (const slug of ['linux','ubuntu','fedora','arch']) {
   const install=await readFile(join(output,'install',slug,'index.html'),'utf8');
   const canonical=`https://stenly.ai/install/${slug}/`;
   assert.ok(install.includes(`href=${canonical} rel=canonical`),`${slug} canonical must use the production domain`);
   assert.ok(install.includes(`content=${canonical} property=og:url`),`${slug} Open Graph URL must use the production domain`);
   assert.doesNotMatch(install,/https:\/\/stenly\.ai\/install\/(?!linux\/|ubuntu\/|fedora\/|arch\/)[^" ]+/,`${slug} internal links must stay same-origin`);
   const anchors=[...install.matchAll(/<a\b[^>]*href=(?:"([^"]*)"|'([^']*)'|([^ >]+))/g)].map(match=>match[1]||match[2]||match[3]);
   const internalLinks=anchors.filter(href=>href.startsWith('/install/')||href.startsWith('https://stenly.ai/install/'));
   assert.ok(internalLinks.length>0,`${slug} should link to other installation instructions`);
   assert.ok(internalLinks.every(href=>href.startsWith('/install/')),`${slug} internal links must be root-relative`);
  }
  const sitemap=await readFile(join(output,'sitemap.xml'),'utf8');
  assert.match(sitemap,/<loc>https:\/\/stenly\.ai\//);
  assert.doesNotMatch(sitemap,/<loc>\/(?!\/)/, 'sitemap locations must be absolute production URLs');
 } finally { await rm(temp,{recursive:true,force:true}); }
});
for (const [name, platform, userAgent, touch, expected] of [
 ['Mac','MacIntel','Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)',0,'macos'],
 ['Linux','Linux x86_64','Mozilla/5.0 (X11; Linux x86_64)',0,'linux'],
 ['Android','Linux armv8l','Mozilla/5.0 (Linux; Android 14; Pixel 8)',5,'other'],
 ['iPad desktop mode','MacIntel','Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)',5,'other'],
 ['ChromeOS','Linux x86_64','Mozilla/5.0 (X11; CrOS x86_64)',0,'other'],
]) {
 test(`OS-aware action: ${name}`,async()=>{
  const page=await browser.newPage({userAgent});
  await page.addInitScript(({platform,touch})=>{
   Object.defineProperty(navigator,'platform',{value:platform});
   Object.defineProperty(navigator,'maxTouchPoints',{value:touch});
   Object.defineProperty(navigator,'userAgentData',{value:undefined});
  },{platform,touch});
  await page.goto(origin);
  assert.equal(await page.locator('html').getAttribute('data-os'),expected);
  const header=page.locator('.nav-link').filter({hasText:'Availability'});
  assert.equal(await header.getAttribute('href'),'/#get-stenly');
  // Every visitor sees the same macOS download link and the Linux "coming soon" note; no OS
  // is ever offered a link to the missing Linux asset.
  assert.equal(await page.locator('#download-macos').getAttribute('href'),MAC_DMG);
  assert.match(await page.locator('#download-macos').innerText(),/Download for macOS/);
  assert.equal(await page.locator('#download-macos [data-os-icon] img').count(),expected==='macos'?1:0);
  assert.match(await page.locator('#linux-status').innerText(),/Linux: coming soon/);
  const hrefs=await page.locator('a').evaluateAll(links=>links.map(a=>a.href));
  assert.ok(hrefs.every(href=>!href.includes('stenly-linux')),`Linux tarball linked for ${name}`);
  assert.equal(await page.locator('.download-picker').count(),0);
  assert.equal(await page.locator('[data-platform]').count(),0);
  await page.close();
 });
}

for (const [width,height] of [[1440,900],[1024,768],[390,844],[320,640]]) {
 test(`pricing page ${width}×${height}`,async()=>{
  const page=await browser.newPage({viewport:{width,height}});const errors=[];
  page.on('pageerror',e=>errors.push(e.message));page.on('response',r=>{if(r.status()>=400)errors.push(`${r.status()} ${r.url()}`)});
  await page.goto(`${origin}/pricing/`);await page.evaluate(()=>document.fonts.ready);
  assert.deepEqual(errors,[]);
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  const text=await page.locator('main').innerText();
  assert.doesNotMatch(text,/not available yet|Desktop release pending/);
  assert.doesNotMatch(text,/Pricing to be announced|Pending/);
  assert.match(text,/Build it yourself: free \(MIT\)/);
  assert.match(text,new RegExp(`Ready to run: \\${PRICE} one-time, one Mac, updates included, ${FREE_MEETINGS} free meetings`));
  assert.match(text,/Linux: free, coming soon/);
  assert.match(text,/Encrypted sharing is available/);
  assert.doesNotMatch(text,/Lifetime updates/i);
  assert.doesNotMatch(text,/most picked|best value|200k|rating|money-back/i);
  assert.equal(await page.getByRole('link',{name:/View source on GitHub/}).getAttribute('href'),REPO);
  assert.equal(await page.getByRole('link',{name:/Download for macOS/}).getAttribute('href'),MAC_DMG);
  assert.doesNotMatch(await page.locator('main').innerHTML(),new RegExp(LINUX_TARBALL.replace(/\./g,'\\.')));
  assert.equal(await page.getByRole('link',{name:/Talk to us/}).count(),1);
  assert.match(await page.getByRole('link',{name:/Talk to us/}).getAttribute('href'),/^mailto:/);
  await page.close();
 });
}
test('Buy link follows checkout_url and is hidden while it is unset',async()=>{
 const temp=await mkdtemp(join(tmpdir(),'stenly-buy-'));
 try {
  const build=async(extra)=>{
   const out=join(temp,extra?'with':'without');
   const cfg=await readFile('config.toml','utf8');
   const cfgPath=join(temp,extra?'with.toml':'without.toml');
   await writeFile(cfgPath,cfg.replace(/^checkout_url = ".*"$/m,`checkout_url = "${extra}"`));
   const r=spawnSync(zola,['--config',cfgPath,'build','--output-dir',out],{encoding:'utf8'});
   assert.equal(r.status,0,r.stdout+r.stderr);
   return readFile(join(out,'pricing','index.html'),'utf8');
  };
  const without=await build('');
  assert.doesNotMatch(without,/buy-licence|Buy a licence/,'no checkout configured: no Buy link');
  const url='https://example.invalid/checkout/stenly';
  const withUrl=await build(url);
  assert.match(withUrl,/Buy a licence/);
  assert.ok(withUrl.includes(`href=${url}`));
 } finally { await rm(temp,{recursive:true,force:true}); }
});

test('the live pricing page links the Polar checkout',async()=>{
 const page=await browser.newPage();await page.goto(`${origin}/pricing/`);
 assert.match(await page.locator('#buy-licence').getAttribute('href'),/^https:\/\/buy\.polar\.sh\//);
 await page.close();
});

test('home page says free trial then $10, and site free_meetings matches the app',async()=>{
 const page=await browser.newPage();await page.goto(origin);
 assert.match(await page.locator('#get-stenly').innerText(),new RegExp(`Free trial, then \\${PRICE}`));
 await page.close();
 const rust=await readFile(join('..','crates','stenly-core','src','license.rs'),'utf8');
 assert.equal(Number(rust.match(/pub const FREE_MEETINGS: u32 = (\d+);/)[1]),FREE_MEETINGS,'FREE_MEETINGS in license.rs must equal free_meetings in site/config.toml');
 assert.match(await readFile('config.toml','utf8'),new RegExp(`free_meetings = ${FREE_MEETINGS}\\b`));
});

test('pricing navigation from homepage',async()=>{
 const page=await browser.newPage();await page.goto(origin);
 await page.getByRole('link',{name:'Pricing',exact:true}).click();
 assert.match(page.url(),/\/pricing\/?$/);assert.equal(await page.getByRole('link',{name:'Pricing',exact:true}).getAttribute('aria-current'),'page');
 await page.close();
});

test('Linux tarball URL is never linked and no undecided price claim appears on any page',async()=>{
 const temp=await mkdtemp(join(tmpdir(),'stenly-claims-'));
 try {
  const output=join(temp,'output');
  const result=spawnSync(zola,['build','--output-dir',output],{encoding:'utf8'});
  assert.equal(result.status,0,result.stdout+result.stderr);
  const pages=['index.html','pricing/index.html',...['linux','ubuntu','fedora','arch'].map(slug=>`install/${slug}/index.html`)];
  for (const file of pages) {
   const html=await readFile(join(output,file),'utf8');
   assert.ok(!html.includes(LINUX_TARBALL),`${file} must not reference the Linux tarball`);
   const hrefs=[...html.matchAll(/<a\b[^>]*\shref=(?:"([^"]*)"|'([^']*)'|([^\s>]+))/g)].map(m=>m[1]||m[2]||m[3]);
   assert.ok(hrefs.every(href=>!/stenly-linux|\/releases\/download\//.test(href)),`${file} links a Linux/versioned asset`);
   assert.ok(!hrefs.some(href=>/\/releases\/latest\/download\//.test(href)&&href!==MAC_DMG),`${file} links an unexpected release asset`);
   const visible=html.replace(/<(script|style)[\s\S]*?<\/\1>/g,' ').replace(/<[^>]+>/g,' ');
   const claim=visible.match(UNDECIDED_CLAIM);
   assert.equal(claim,null,`${file} makes an undecided price claim: ${claim?.[0]}`);
   if (file==='pricing/index.html') assert.match(visible,/\$10/);
   else assert.ok(!/\$10/.test(visible)||file==='index.html',`${file} should not mention the price`);
  }
  const sitemap=await readFile(join(output,'sitemap.xml'),'utf8');
  assert.doesNotMatch(sitemap,/stenly-linux/);
 } finally { await rm(temp,{recursive:true,force:true}); }
});
