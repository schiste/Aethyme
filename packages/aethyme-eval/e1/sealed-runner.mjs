// Generic caller for the independently frozen observer. No expected repair.
import fs from 'node:fs';
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';

const [file, observer, modulePath, executablePath] = process.argv.slice(2);
const source = fs.readFileSync(file);
assert(source.length <= 256 * 1024, 'candidate byte limit');
const {inspect} = await import(pathToFileURL(observer).href);
const {chromium} = await import(pathToFileURL(modulePath).href);
const browser = await chromium.launch({headless:true, executablePath});
const errors = [], requests = [], sockets = [];
const started = performance.now();
let result;
try {
  const context = await browser.newContext({viewport:{width:390,height:844},serviceWorkers:'block'});
  await context.route('**/*', async route => {
    requests.push(route.request().url());
    await route.abort('blockedbyclient');
  });
  await context.routeWebSocket('**/*', socket => {sockets.push(socket.url()); socket.close();});
  const page = await context.newPage();
  page.setDefaultTimeout(3000);
  page.on('pageerror', error => errors.push(String(error)));
  await page.setContent(source.toString('utf8'), {waitUntil:'load',timeout:3000});
  result = await inspect(page);
  assert.equal(result.schema, 'aethyme.e1.sealed-observation/v1');
  assert.equal(typeof result.passed, 'boolean');
  assert(Array.isArray(result.checks) && result.checks.length > 0);
  assert(result.checks.every(check => typeof check.name === 'string' && typeof check.passed === 'boolean'));
  assert.equal(new Set(result.checks.map(check=>check.name)).size,result.checks.length);
  assert.equal(result.passed,result.checks.every(check=>check.passed));
  // Observe asynchronous activity for a declared finite window after inspect.
  await page.waitForTimeout(250);
  result.browser_version = browser.version();
  result.errors = errors;
  result.requests = requests;
  result.sockets = sockets;
} finally {
  await browser.close();
}
// Keep event listeners live through teardown, then freeze the verdict.
result.checks.push({name:'caller has no runtime page errors',passed:errors.length===0});
result.checks.push({name:'caller has no external requests',passed:requests.length===0 && sockets.length===0});
result.passed = result.checks.every(check=>check.passed);
result.observation_window = {post_inspect_ms:250,wall_ms:performance.now()-started,closed:true};
console.log(JSON.stringify(result));
process.exitCode = result.passed ? 0 : 1;
