import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { chromium } from '@playwright/test';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const origin = 'http://127.0.0.1:1112';
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
  assert.equal(await page.locator('.source-link').innerText(), 'Public source pending');
  assert.equal(await page.locator('.nav-link').filter({hasText:'Availability'}).count(),1);
  assert.equal(await page.locator('.source-link').evaluate(el=>el.tagName), 'SPAN');
  assert.match(await page.locator('#downloads-unavailable').innerText(),/Public source publication and desktop installers are not available yet/);
  assert.equal(await page.locator('.download-picker').count(),0);
  assert.equal(await page.locator('.platform-link').count(),0);
  assert.equal(await page.locator('.source-link').evaluate(el=>el.closest('a')),null);
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
test('unavailable source/downloads are not linked, and sharing copy is accurate without JavaScript',async()=>{
 const context=await browser.newContext({javaScriptEnabled:false});const page=await context.newPage();await page.goto(origin);
 assert.equal(await page.locator('.source-link').evaluate(el=>el.tagName),'SPAN');
 assert.equal(await page.locator('.download-picker').count(),0);
 assert.equal(await page.locator('[data-platform]').count(),0);
 assert.match(await page.locator('#downloads-unavailable').innerText(),/Public source publication and desktop installers are not available yet/);
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
  assert.match(await page.locator('.install-availability').innerText(),/Public desktop installers are not available yet/);
  assert.match(await page.locator('.install-availability').innerText(),/cannot complete an installation from them today/);
  if(path==='/install/linux/'){
   const guide=await page.locator('main').innerText();
   assert.match(guide,/Public Linux installers are not available yet/);
   assert.doesNotMatch(await page.locator('main').innerHTML(),/github\.com\/wbunting\/stenly\/releases\/latest\/download\/stenly-linux\.tar\.gz/);
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
  assert.match(html,/Public source pending/);
  assert.match(html,/Public source publication and desktop installers are not available yet/);
  assert.match(html,/Encrypted sharing is available/);
  assert.match(html,/your configured server/);
  assert.match(html,/href=https:\/\/share\.stenly\.ai/);
  assert.doesNotMatch(html,/Coming soon|Download for macOS|Get it for Linux|github\.com\/wbunting\/stenly|build-from-source/);
  assert.match(html,/href=https:\/\/stenly\.ai\/ rel=canonical/);
  assert.match(html,/content=https:\/\/stenly\.ai\/ property=og:url/);
  assert.equal((html.match(/<span class=source-link>/g)||[]).length,1);
  assert.equal((html.match(/class=nav-link href=\/#get-stenly/g)||[]).length,1);
  assert.doesNotMatch(html,/href=["']?https:\/\/github\.com\/wbunting\/stenly/);
  for (const asset of ['/site.css','/site.js','/fonts/dm-sans.woff2','/fonts/instrument-serif.woff2','/favicon.svg','/icons/apple.svg','/icons/linux.svg','href=/pricing/','href=/#get-stenly','share.stenly.ai']) assert.ok(html.includes(asset),`missing same-origin asset/link: ${asset}`);
  assert.doesNotMatch(html,/https:\/\/stenly\.ai\/(site\.css|site\.js|fonts\/|favicon\.svg|icons\/)/);
  assert.doesNotMatch(html,/https:\/\/stenly\.ai\/(pricing|install)\//,"homepage internal links must stay relative to the preview host");
  assert.doesNotMatch(html,/https:\/\/github\.com\/wbunting\/stenly/,"homepage must not expose the private source repo URL");
  const pricing=await readFile(join(output,'pricing','index.html'),'utf8');
  assert.match(pricing,/href=https:\/\/stenly\.ai\/pricing\/ rel=canonical/);
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
  if(expected==='macos') {
   assert.equal(await header.getAttribute('href'),'/#get-stenly');
  } else {
   assert.equal(await header.innerText(),'Availability');
  }
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
  assert.match(text,/Public source publication is not available yet/);
  assert.match(text,/Public desktop installers are not available yet/);
  assert.match(text,/Pricing and licence details are still being finalized/);
  assert.match(text,/Encrypted sharing is available/);
  assert.doesNotMatch(text,/One machine|Lifetime updates|10 meetings/);
  assert.doesNotMatch(text,/most picked|best value|200k|rating|money-back/i);
  assert.doesNotMatch(await page.locator('main').innerHTML(),/href=["']?https:\/\/github\.com\/wbunting\/stenly/);
  assert.equal(await page.getByRole('link',{name:/Talk to us/}).count(),1);
  assert.match(await page.getByRole('link',{name:/Talk to us/}).getAttribute('href'),/^mailto:/);
  await page.close();
 });
}
test('pricing navigation from homepage',async()=>{
 const page=await browser.newPage();await page.goto(origin);
 await page.getByRole('link',{name:'Pricing',exact:true}).click();
 assert.match(page.url(),/\/pricing\/?$/);assert.equal(await page.getByRole('link',{name:'Pricing',exact:true}).getAttribute('aria-current'),'page');
 await page.close();
});
