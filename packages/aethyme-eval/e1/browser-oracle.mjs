// Independent browser observer. It reads only candidate bytes and the frozen
// public requirement selection; it never supplies a candidate or merge patch.
import fs from 'node:fs';
import assert from 'node:assert/strict';
import { pathToFileURL, fileURLToPath } from 'node:url';

export async function observe(file, requirements, modulePath, executablePath) {
  const source = fs.readFileSync(file);
  assert(source.length <= 256 * 1024, 'candidate byte limit');
  const { chromium } = await import(pathToFileURL(modulePath).href);
  const browser = await chromium.launch({headless:true,executablePath});
  const requests = [], errors = [], checks = [];
  const start = performance.now();
  const check = async (name, body) => {
    try { await body(); checks.push({name,passed:true}); }
    catch (error) { checks.push({name,passed:false,error:String(error)}); }
  };
  try {
    const context = await browser.newContext({viewport:{width:390,height:844},serviceWorkers:'block',extraHTTPHeaders:{'Accept-Language':'en-US'}});
    const sockets = [];
    await context.routeWebSocket('**/*', socket => {
      sockets.push(socket.url());
      socket.close();
    });
    const page = await context.newPage();
    const browserUserAgent = await page.evaluate(()=>navigator.userAgent);
    page.setDefaultTimeout(3000);
    page.on('pageerror', error => errors.push(String(error)));
    await context.route('**/*', async route => {
      const request = route.request();
      requests.push({url:request.url(),method:request.method(),headers:request.headers(),body:request.postData()});
      if (request.url().startsWith('https://telemetry.invalid/')) {
        await route.fulfill({status:204,body:''});
      } else {
        await route.abort('blockedbyclient');
      }
    });
    await page.setContent(source.toString('utf8'), {waitUntil:'load',timeout:3000});
    if (requirements.some(requirement=>['consent','privacy','no_analytics'].includes(requirement))) {
      await check('consent is initially false', async () => assert.equal(await page.locator('#consent').isChecked(),false));
    }
    await check('unique DOM identities', async () => {
      const ids = await page.locator('[id]').evaluateAll(elements => elements.map(e => e.id));
      assert.equal(new Set(ids).size, ids.length);
    });
    if (requirements.includes('label')) await check('selected heading appears exactly once', async () => {
      assert.equal(await page.getByRole('heading',{name:'Directory search',exact:true}).count(),1);
    });
    if (requirements.includes('accessible')) await check('query has selected accessible name', async () => {
      assert.equal(await page.getByRole('textbox',{name:'Find a place',exact:true}).count(),1);
    });
    if (requirements.includes('moved')) await check('unique panel moved to sidebar', async () => {
      assert.equal(await page.locator('#sidebar #search-panel').count(),1);
      assert.equal(await page.locator('#main #search-panel').count(),0);
    });
    if (requirements.includes('mobile')) await check('mobile control focuses the same query', async () => {
      await page.getByRole('button',{name:'Open search',exact:true}).click();
      assert.equal(await page.locator('#query').evaluate(e=>e===document.activeElement),true);
    });
    if (requirements.includes('keyboard')) await check('Escape clears the query', async () => {
      await page.locator('#query').fill('independent observer value');
      await page.locator('#query').press('Escape');
      assert.equal(await page.locator('#query').inputValue(),'');
    });
    if (requirements.includes('search')) {
      await check('Enter preserves directory search', async () => {
        await page.locator('#query').fill('La');
        await page.locator('#query').press('Enter');
        assert.equal(await page.locator('#results').textContent(),'Lake');
      });
      await check('pointer preserves directory search', async () => {
        await page.locator('#query').fill('For');
        await page.getByRole('button',{name:'Search',exact:true}).click();
        assert.equal(await page.locator('#results').textContent(),'Forest');
      });
    }
    if (requirements.includes('consent') || requirements.includes('privacy')) {
      await check('no telemetry without affirmative consent', async () => {
        assert.equal(await page.locator('#consent').isChecked(),false);
        assert.deepEqual(requests,[],'load and earlier searches require consent too');
        await page.locator('#consent').uncheck();
        const before = requests.length;
        await page.locator('#query').fill('observer private search');
        await page.locator('#query').press('Enter');
        await page.waitForTimeout(250);
        assert.equal(requests.length,before);
      });
      await check('one anonymous event with consent', async () => {
        await page.locator('#consent').check();
        const before = requests.length;
        await page.locator('#query').fill('La');
        await page.locator('#query').press('Enter');
        assert.equal(await page.locator('#results').textContent(),'Lake');
        await page.waitForTimeout(500);
        assert.equal(requests.length-before,1);
        const event = requests[before];
        assert.equal(event.url,'https://telemetry.invalid/event');
        assert.equal(event.method,'POST');
        const permittedHeaders = new Set(['accept','accept-language','content-type','origin','referer','user-agent','sec-ch-ua','sec-ch-ua-mobile','sec-ch-ua-platform','sec-fetch-dest','sec-fetch-mode','sec-fetch-site']);
        assert.equal(Object.keys(event.headers).every(key=>permittedHeaders.has(key)),true,'no application data headers');
        assert.equal(event.headers.accept,'*/*');
        assert.equal(event.headers['accept-language'],'en-US');
        assert.equal(event.headers['user-agent'],browserUserAgent);
        assert.equal(event.headers.referer,undefined);
        assert.equal(event.headers.origin,'null');
        assert.equal(['text/plain;charset=UTF-8','application/json'].includes(event.headers['content-type']),true);
        const payload = JSON.parse(event.body);
        assert.equal(payload.event,'search');
        assert.deepEqual(Object.keys(payload),['event']);
        assert.equal(JSON.stringify(event).includes('observer private search'),false);
        assert.equal(JSON.stringify(event).includes(encodeURIComponent('observer private search')),false);
        assert.equal(JSON.stringify(event).includes(Buffer.from('observer private search').toString('base64')),false);
        assert.equal(new URL(event.url).search,'');
      });
      await check('consent transport is independent of query', async () => {
        const previous = requests.at(-1);
        const before = requests.length;
        await page.locator('#query').fill('For');
        await page.getByRole('button',{name:'Search',exact:true}).click();
        assert.equal(await page.locator('#results').textContent(),'Forest');
        await page.waitForTimeout(500);
        assert.equal(requests.length-before,1);
        const current = requests[before];
        assert.deepEqual(current,previous);
      });
      await check('revoked consent stops later events', async () => {
        await page.locator('#consent').uncheck();
        const before = requests.length;
        await page.locator('#query').fill('Mu');
        await page.locator('#query').press('Enter');
        assert.equal(await page.locator('#results').textContent(),'Museum');
        await page.waitForTimeout(250);
        assert.equal(requests.length,before);
      });
    }
    if (requirements.includes('navigation')) await check('existing navigation survives', async () => {
      await page.getByRole('link',{name:'Results',exact:true}).click();
      assert.equal(new URL(page.url()).hash,'#results');
    });
    if (requirements.includes('no_analytics')) await check('subtraction has no analytics even with consent', async () => {
      assert.equal(await page.locator('#consent').isChecked(),false);
      assert.deepEqual(requests,[]);
      for (const consent of [true,false]) {
        await page.locator('#consent').setChecked(consent);
        await page.locator('#query').fill(consent?'La':'For');
        if(consent) await page.locator('#query').press('Enter');
        else await page.getByRole('button',{name:'Search',exact:true}).click();
        assert.equal(await page.locator('#results').textContent(),consent?'Lake':'Forest');
        await page.waitForTimeout(500);
        assert.deepEqual(requests,[]);
      }
    });
    if (requirements.includes('consumer_footer')) await check('actual retained consumer survives', async () => {
      assert.equal(await page.locator('#consumer-note').count(),1);
      assert.equal(await page.locator('#consumer-note').textContent(),'Consumer observes the consent-aware search revision.');
    });
    if (!requirements.includes('consent') && !requirements.includes('privacy')) {
      await check('ordinary compositions emit no telemetry', async () => assert.deepEqual(requests,[]));
    }
    await check('no WebSocket egress', async () => assert.deepEqual(sockets,[]));
    await check('no runtime page errors', async () => assert.deepEqual(errors,[]));
    return {schema:'aethyme.e1.browser-observation/v1',passed:checks.every(c=>c.passed),checks,requests,sockets,errors,observation_window_ms:500,wall_ms:performance.now()-start,browser_version:browser.version()};
  } finally { await browser.close(); }
}
if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  try {
    const result = await observe(process.argv[2],process.argv[3].split(',').filter(Boolean),process.argv[4],process.argv[5]);
    process.stdout.write(JSON.stringify(result)+'\n');
    process.exitCode = result.passed ? 0 : 1;
  } catch (error) {
    process.stdout.write(JSON.stringify({schema:'aethyme.e1.browser-observation/v1',passed:false,infrastructure_error:String(error)})+'\n');
    process.exitCode=2;
  }
}
