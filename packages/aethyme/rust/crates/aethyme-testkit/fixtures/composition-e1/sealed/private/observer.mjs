// Independent requirements observer designed before engine selection.
// Caller owns browser/network isolation; this module never creates candidate bytes.
import assert from 'node:assert/strict';
export async function inspect(page) {
  const checks = [];
  const check = async (name, action) => { try { await action(); checks.push({name,passed:true}); } catch (error) { checks.push({name,passed:false,error:String(error)}); } };
  await check('all identities unique', async () => {
    const ids = await page.locator('[id]').evaluateAll(nodes => nodes.map(node => node.id));
    assert.equal(ids.length, new Set(ids).size);
  });
  await check('loan panel moved without duplicate or similar-panel substitution', async () => {
    assert.equal(await page.locator('#drawer > #loan-panel').count(),1);
    assert.equal(await page.locator('#primary #loan-panel').count(),0);
    assert.equal(await page.locator('#primary > #summary-panel').count(),1);
    assert.equal(await page.locator('#drawer #summary-panel').count(),0);
    assert.equal(await page.locator('#summary-query').getAttribute('aria-label'),'Summary title fragment');
    assert.equal(await page.locator('#summary-submit').textContent(),'Find summary');
    assert.equal(await page.locator('#summary-matches').textContent(),'No summary selected');
  });
  await check('selected accessible edit belongs to moved loan input', async () => {
    const selected = page.getByRole('textbox',{name:'Find a title',exact:true});
    assert.equal(await selected.count(),1);
    assert.equal(await selected.getAttribute('id'),'loan-query');
    assert.equal(await page.locator('#drawer #loan-submit').textContent(),'Find loans');
  });
  await check('keyboard search executes actual moved form', async () => {
    await page.locator('#loan-query').fill('slATE');
    await page.locator('#loan-query').press('Enter');
    assert.equal(await page.locator('#loan-matches').textContent(),'Slate Atlas');
  });
  await check('Escape clears both moved query and output', async () => {
    await page.locator('#loan-query').press('Escape');
    assert.equal(await page.locator('#loan-query').inputValue(),'');
    assert.equal(await page.locator('#loan-matches').textContent(),'');
  });
  await check('pointer search executes actual moved form', async () => {
    await page.locator('#loan-query').fill('CEDAR');
    await page.getByRole('button',{name:'Find loans',exact:true}).click();
    assert.equal(await page.locator('#loan-matches').textContent(),'Cedar Almanac');
    assert.equal(await page.locator('#summary-matches').textContent(),'No summary selected');
  });
  await check('catalog navigation remains real', async () => {
    await page.getByRole('link',{name:'Catalog',exact:true}).click();
    assert.equal(new URL(page.url()).hash,'#catalog');
    assert.equal((await page.locator('#catalog h2').textContent()).trim(),'Public catalog');
    assert.equal((await page.locator('#catalog p').textContent()).replace(/\s+/g,' ').trim(),'Slate Atlas; Cedar Almanac; Amber Reader');
  });
  return {schema:'aethyme.e1.sealed-observation/v1',passed:checks.every(check=>check.passed),checks};
}
