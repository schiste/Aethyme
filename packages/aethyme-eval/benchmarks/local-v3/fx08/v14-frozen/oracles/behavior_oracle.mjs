#!/usr/bin/env node
import fs from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const task = process.argv[process.argv.indexOf("--task") + 1];
const repo = path.resolve(process.argv[process.argv.indexOf("--repo") + 1]);
const playwrightPath = process.env.FX08_PLAYWRIGHT_MODULE;
const executablePath = process.env.FX08_BROWSER_EXECUTABLE;

function legacy(taskId) {
  const result = spawnSync(process.execPath, [path.join(here, "legacy_dom_oracle.mjs"), "--task", taskId, "--repo", repo], { encoding: "utf8" });
  if (result.status !== 0) throw new Error(result.stderr || result.stdout || `legacy oracle failed (${result.status})`);
  return JSON.parse(result.stdout.trim().split(/\r?\n/).at(-1));
}

function contentType(file) {
  const ext = path.extname(file).toLowerCase();
  return ({ ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".mjs": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8", ".json": "application/json; charset=utf-8", ".svg": "image/svg+xml" })[ext] || "application/octet-stream";
}

async function startServer(root) {
  const resolvedRoot = path.resolve(root);
  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url || "/", "http://127.0.0.1");
      const pathname = decodeURIComponent(url.pathname);
      const rel = pathname === "/" ? "index.html" : `.${pathname}`;
      const file = path.resolve(resolvedRoot, rel);
      if (file !== resolvedRoot && !file.startsWith(resolvedRoot + path.sep)) {
        res.writeHead(403).end("forbidden");
        return;
      }
      const contents = await fs.readFile(file);
      res.writeHead(200, { "content-type": contentType(file), "cache-control": "no-store" });
      res.end(contents);
    } catch {
      res.writeHead(404).end("not found");
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  return { server, url: `http://127.0.0.1:${address.port}/` };
}

async function browserPage(taskId, repoPath, viewport, body) {
  if (!playwrightPath || !executablePath) throw new Error("FX08 Playwright module or browser executable is not configured");
  const require = createRequire(import.meta.url);
  const { chromium } = require(playwrightPath);
  const packageJson = require(path.join(playwrightPath, "package.json"));
  const { server, url } = await startServer(repoPath);
  let browser;
  try {
    browser = await chromium.launch({ headless: true, executablePath });
    const page = await browser.newPage({ viewport });
    const pageErrors = [];
    page.on("pageerror", error => pageErrors.push(String(error)));
    await page.addInitScript(() => {
      const key = "__fx08_document_boot_count";
      sessionStorage.setItem(key, String(Number(sessionStorage.getItem(key) || "0") + 1));
    });
    await page.goto(url, { waitUntil: "load", timeout: 10000 });
    const checks = await body(page);
    checks.application_errors = pageErrors;
    checks.browser = { playwright: packageJson.version, chrome: browser.version(), viewport };
    return checks;
  } finally {
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}

async function navigationFree(page, action) {
  let navigated = false;
  const navigation = page.waitForNavigation({ waitUntil: "domcontentloaded", timeout: 700 }).then(() => { navigated = true; }).catch(() => {});
  await action();
  await navigation;
  return !navigated;
}

async function listText(locator) {
  return (await locator.allTextContents()).map(value => value.trim()).filter(Boolean);
}

async function runP01(repoPath) {
  const checks = await browserPage("P01", repoPath, { width: 1280, height: 800 }, async page => {
    const form = page.getByRole("search").first();
    const formCount = await page.getByRole("search").count();
    const searchbox = page.getByRole("searchbox").first();
    const searchboxCount = await page.getByRole("searchbox").count();
    const hasSearchbox = searchboxCount === 1;
    const labelAssociated = hasSearchbox && await searchbox.evaluate(el => {
      const labels = el.labels ? [...el.labels] : [];
      const ariaLabel = el.getAttribute("aria-label")?.trim() || "";
      const labelledBy = el.getAttribute("aria-labelledby");
      return labels.length > 0 && (!!ariaLabel || !!labelledBy || labels.some(label => label.textContent.trim()));
    });
    const typeSearch = hasSearchbox && await searchbox.evaluate(el => el.type === "search");
    const inputId = hasSearchbox ? await searchbox.getAttribute("id") : null;
    const searchButton = form.getByRole("button", { name: /search/i }).first();
    const buttonCount = await form.getByRole("button", { name: /search/i }).count();
    const results = page.locator("#catalog-results li");
    const beforeUrl = page.url();

    let buttonFilters = false;
    let buttonNoNavigation = false;
    if (hasSearchbox && buttonCount > 0) {
      await searchbox.fill("moss");
      buttonNoNavigation = await navigationFree(page, () => searchButton.click());
      const names = await listText(results);
      buttonFilters = names.length === 1 && names[0].toLowerCase() === "moss";
    }

    let enterFilters = false;
    let enterNoNavigation = false;
    if (hasSearchbox) {
      const currentBox = page.getByRole("searchbox").first();
      await currentBox.fill("fern");
      enterNoNavigation = await navigationFree(page, () => currentBox.press("Enter"));
      const names = (await listText(results)).map(value => value.toLowerCase());
      enterFilters = names.includes("fern") && names.includes("copper fern") && names.length === 2;
    }

    const stayedOnDocument = page.url() === beforeUrl && await page.evaluate(() => sessionStorage.getItem("__fx08_document_boot_count") === "1");
    const shortcut = async modifier => {
      await page.evaluate(() => { if (document.activeElement) document.activeElement.blur(); });
      await page.keyboard.press(`${modifier}+k`);
      return hasSearchbox && !!inputId && await page.evaluate(id => document.activeElement?.id === id, inputId);
    };
    const ctrlFocuses = await shortcut("Control");
    const metaFocuses = await shortcut("Meta");
    return {
      task_id: "P01",
      task_pass: formCount === 1 && labelAssociated && typeSearch && buttonFilters && enterFilters && buttonNoNavigation && enterNoNavigation && stayedOnDocument && ctrlFocuses && metaFocuses,
      decision_survived: inputId === "global-search" && ctrlFocuses && metaFocuses,
      checks: {
        search_landmark: formCount === 1,
        associated_accessible_label: labelAssociated,
        searchbox_type: typeSearch,
        button_submission_filters_without_navigation: buttonFilters && buttonNoNavigation,
        enter_submission_filters_without_navigation: enterFilters && enterNoNavigation,
        no_document_navigation: stayedOnDocument,
        global_search_id_preserved: inputId === "global-search",
        ctrl_k_focuses_search: ctrlFocuses,
        meta_k_focuses_search: metaFocuses
      }
    };
  });
  return { ...checks, decision_survived: !!checks.decision_survived };
}

async function runP02(repoPath) {
  return browserPage("P02", repoPath, { width: 375, height: 812 }, async page => {
    const primary = page.locator("#primary-query");
    const primarySearch = page.locator("#primary-search-button");
    const primaryResults = page.locator("#primary-results li");
    const quick = page.locator("#quick-query");
    const quickSearch = page.locator("#quick-search-button");
    const quickResults = page.locator("#quick-results li");
    let primaryResultProduced = false;
    let quickResultProduced = false;
    if (await primary.count() === 1 && await primarySearch.count() === 1 && await quick.count() === 1 && await quickSearch.count() === 1) {
      await primary.fill("fern");
      await primarySearch.click();
      primaryResultProduced = (await listText(primaryResults)).some(name => name.toLowerCase() === "fern");
      await quick.fill("moss");
      await quickSearch.click();
      quickResultProduced = (await listText(quickResults)).some(name => name.toLowerCase() === "moss");
    }
    const quickBeforePrimary = await listText(quickResults);
    const quickInputBeforePrimary = await quick.inputValue();
    let quickIndependentAfterPrimary = false;
    if (quickBeforePrimary.some(name => name.toLowerCase() === "moss") && quickInputBeforePrimary === "moss") {
      await primary.fill("amber");
      await primarySearch.click();
      quickIndependentAfterPrimary =
        JSON.stringify(await listText(quickResults)) === JSON.stringify(quickBeforePrimary) &&
        await quick.inputValue() === quickInputBeforePrimary;
    }
    const clearButton = page.getByRole("button", { name: /clear/i }).first();
    const clearCount = await page.getByRole("button", { name: /clear/i }).count();
    let clearBehavior = false;
    let quickAfterClearMatches = false;
    if (clearCount > 0) {
      await clearButton.click();
      const inputEmpty = await primary.inputValue() === "";
      const resultsEmpty = await primaryResults.count() === 0;
      const focusRestored = await primary.evaluate(el => document.activeElement === el);
      quickAfterClearMatches =
        JSON.stringify(await listText(quickResults)) === JSON.stringify(quickBeforePrimary) &&
        await quick.inputValue() === quickInputBeforePrimary;
      clearBehavior = inputEmpty && resultsEmpty && focusRestored;
    }
    const decisionSurvived = quickIndependentAfterPrimary &&
      (clearCount === 0 || quickAfterClearMatches);
    const layout = await page.evaluate(() => {
      const width = window.innerWidth;
      const elements = [
        { name: "primary-query", el: document.querySelector("#primary-query") },
        { name: "primary-search-button", el: document.querySelector("#primary-search-button") },
        { name: "clear-button", el: [...document.querySelectorAll("button")].find(el => /clear/i.test(el.innerText + " " + (el.getAttribute("aria-label") || ""))) }
      ];
      const boxes = elements.map(({ name, el }) => {
        if (!el) return { name, present: false };
        const rect = el.getBoundingClientRect();
        const style = getComputedStyle(el);
        return { name, present: true, visible: rect.width > 0 && rect.height > 0 && style.visibility !== "hidden" && style.display !== "none", left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
      });
      let overlap = false;
      for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) {
        const a = boxes[i], b = boxes[j];
        if (a.present && b.present && a.left < b.right - 1 && a.right > b.left + 1 && a.top < b.bottom - 1 && a.bottom > b.top + 1) overlap = true;
      }
      return { viewport_width: width, document_width: document.documentElement.scrollWidth, boxes, overlap };
    });
    const controlsFit = layout.document_width <= layout.viewport_width && layout.boxes.length === 3 && layout.boxes.every(box => box.present && box.visible && box.left >= -1 && box.right <= layout.viewport_width + 1) && !layout.overlap;
    return {
      task_id: "P02",
      task_pass: primaryResultProduced && quickResultProduced && clearBehavior && controlsFit,
      decision_survived: decisionSurvived,
      checks: {
        primary_search_works: primaryResultProduced,
        quick_find_works: quickResultProduced,
        narrow_viewport_controls_fit_without_horizontal_overflow: controlsFit,
        clear_empties_primary_query_results_and_restores_focus: clearBehavior,
        quick_find_query_and_results_remain_independent_after_primary_actions: decisionSurvived,
        narrow_layout: layout
      },
      browser: { playwright: process.env.FX08_PLAYWRIGHT_VERSION || null, chrome: process.env.FX08_BROWSER_VERSION || null, viewport: { width: 375, height: 812 } },
      application_errors: []
    };
  });
}

async function category(page, label) {
  const selects = page.getByRole("combobox");
  if (await selects.count()) {
    const select = selects.first();
    const options = await select.locator("option").evaluateAll(nodes => nodes.map(node => ({ value: node.value, label: node.label.trim() })));
    const wanted = options.find(option => option.label.toLowerCase() === label.toLowerCase() || option.value.toLowerCase() === label.toLowerCase());
    if (wanted) { await select.selectOption(wanted.value); return true; }
  }
  for (const role of ["radio", "checkbox", "tab", "button", "link"]) {
    const control = page.getByRole(role, { name: new RegExp(`^${label}$`, "i") }).first();
    if (await page.getByRole(role, { name: new RegExp(`^${label}$`, "i") }).count()) {
      if (role === "radio" || role === "checkbox") await control.check(); else await control.click();
      return true;
    }
  }
  return false;
}

async function listboxNames(page) {
  return page.getByRole("listbox").first().locator('[role="option"]').allTextContents().then(values => values.map(value => value.trim()).filter(Boolean));
}

async function activeName(page) {
  return page.getByRole("listbox").first().evaluate(list => {
    const id = list.getAttribute("aria-activedescendant");
    return id ? document.getElementById(id)?.textContent?.trim() || null : null;
  });
}

async function runP03(repoPath) {
  return browserPage("P03", repoPath, { width: 1280, height: 800 }, async page => {
    const list = page.getByRole("listbox").first();
    const listExists = await page.getByRole("listbox").count() === 1;
    if (listExists) { await list.focus(); await page.keyboard.press("ArrowDown"); }
    const afterArrow = await activeName(page);
    const plantsControl = await category(page, "Plants");
    const plants = await listboxNames(page);
    const activeAfterPlants = await activeName(page);
    const mineralsControl = await category(page, "Minerals");
    const minerals = await listboxNames(page);
    const activeAfterMinerals = await activeName(page);
    if (listExists) { await list.focus(); await page.keyboard.press("Enter"); }
    const selection = (await page.locator("#selection").textContent() || "").trim();
    const allControl = await category(page, "All");
    const all = await listboxNames(page);
    const plantsCorrect = plants.map(x => x.toLowerCase()).sort().join("|") === ["fern", "moss"].sort().join("|");
    const mineralsCorrect = minerals.map(x => x.toLowerCase()).sort().join("|") === ["amber", "blue slate"].sort().join("|");
    const allCorrect = all.map(x => x.toLowerCase()).sort().join("|") === ["fern", "moss", "amber", "blue slate"].sort().join("|");
    const retained = plantsCorrect && afterArrow?.toLowerCase() === "moss" && activeAfterPlants?.toLowerCase() === "moss";
    const fallbackAndEnter = mineralsCorrect && activeAfterMinerals?.toLowerCase() === minerals[0]?.toLowerCase() && activeAfterMinerals?.toLowerCase() === "amber" && selection.toLowerCase().includes("amber");
    return {
      task_id: "P03",
      task_pass: listExists && plantsControl && mineralsControl && allControl && plantsCorrect && mineralsCorrect && allCorrect && afterArrow?.toLowerCase() === "moss" && fallbackAndEnter,
      decision_survived: retained,
      checks: {
        plants_filter: plantsCorrect,
        minerals_filter: mineralsCorrect,
        all_filter_restores_full_results: allCorrect,
        arrow_navigation_moves_active_result: afterArrow?.toLowerCase() === "moss",
        active_result_retained_when_visible: retained,
        active_result_resets_when_hidden_and_enter_selects_first: fallbackAndEnter,
        category_controls_found: { plants: plantsControl, minerals: mineralsControl, all: allControl },
        selected_status: selection
      },
      application_errors: []
    };
  });
}

async function runP04(repoPath) {
  return browserPage("P04", repoPath, { width: 1280, height: 800 }, async page => {
    const input = page.getByRole("spinbutton", { name: /quantity/i }).first();
    const inputCount = await page.getByRole("spinbutton", { name: /quantity/i }).count();
    const cart = page.locator("#cart-count");
    const initialCartText = (await cart.textContent() || "").trim();
    const initialCount = Number(initialCartText.match(/-?\d+(?:\.\d+)?/)?.[0]);
    const inputInfo = inputCount ? await input.evaluate(el => ({ min: el.min, max: el.max, required: el.required, form: !!el.form })) : null;
    const min = inputInfo?.min === "" ? 1 : Number(inputInfo?.min);
    const max = inputInfo?.max === "" ? 5 : Number(inputInfo?.max);
    const validQty = Math.min(max, Math.max(min, 2));
    if (inputCount) {
      await page.evaluate(() => {
        window.__fx08_submit_count = 0;
        document.addEventListener("submit", () => { window.__fx08_submit_count += 1; }, true);
      });
      await input.fill(String(validQty));
      await input.focus();
      await page.keyboard.press("Alt+Enter");
      const afterValid = Number(((await cart.textContent()) || "").match(/-?\d+(?:\.\d+)?/)?.[0]);
      await input.evaluate(el => el.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", altKey: true, repeat: true, bubbles: true, cancelable: true })));
      const afterRepeat = Number(((await cart.textContent()) || "").match(/-?\d+(?:\.\d+)?/)?.[0]);
      const submitsAfterRepeat = await page.evaluate(() => window.__fx08_submit_count);

      const invalidCases = [];
      for (const [name, value, flag] of [
        ["required", "", "valueMissing"],
        ["min", String(min - 1), "rangeUnderflow"],
        ["max", String(max + 1), "rangeOverflow"]
      ]) {
        await input.fill(value);
        const invalidity = await input.evaluate((el, key) => ({ valid: el.validity.valid, expectedFlag: !!el.validity[key] }), flag);
        const beforeInvalid = Number(((await cart.textContent()) || "").match(/-?\d+(?:\.\d+)?/)?.[0]);
        await input.focus();
        await page.keyboard.press("Alt+Enter");
        const afterInvalid = Number(((await cart.textContent()) || "").match(/-?\d+(?:\.\d+)?/)?.[0]);
        invalidCases.push({ name, invalidity, cart_unchanged: beforeInvalid === afterInvalid });
      }
      const noNavigation = await page.evaluate(() => sessionStorage.getItem("__fx08_document_boot_count") === "1");
      const validOnce = Number.isFinite(initialCount) && afterValid === initialCount + validQty;
      const repeatBlocked = afterRepeat === afterValid && submitsAfterRepeat === 1;
      const invalidCasesBlocked = invalidCases.every(value => !value.invalidity.valid && value.invalidity.expectedFlag && value.cart_unchanged);
      const invalidBlocked = invalidCasesBlocked;
      const nativeSubmit = inputInfo?.form && submitsAfterRepeat === 1;
      return {
        task_id: "P04",
        task_pass: inputCount === 1 && validOnce && repeatBlocked && invalidBlocked && nativeSubmit && noNavigation,
        decision_survived: inputCount === 1 && invalidBlocked,
        checks: {
          valid_alt_enter_updates_cart_once: validOnce,
          repeated_keydown_does_not_add_again: repeatBlocked,
          native_submit_event_observed: !!nativeSubmit,
          required_min_max_invalid_cases_block_cart: invalidCasesBlocked,
          invalid_cases: invalidCases,
          no_document_navigation: noNavigation,
          quantity_constraints: inputInfo
        },
        application_errors: []
      };
    }
    return { task_id: "P04", task_pass: false, decision_survived: false, checks: { quantity_spinbutton_with_label: false }, application_errors: [] };
  });
}

async function main() {
  if (["H01", "H02"].includes(task)) {
    const result = legacy(task);
    console.log(JSON.stringify(result));
    return;
  }
  const handlers = { P01: runP01, P02: runP02, P03: runP03, P04: runP04 };
  if (!handlers[task]) throw new Error(`unknown task id ${task}`);
  let result;
  try {
    result = await handlers[task](repo);
  } catch (error) {
    result = { task_id: task, task_pass: false, decision_survived: false, checks: {}, application_errors: [String(error.stack || error)] };
  }
  console.log(JSON.stringify(result));
}

main().catch(error => { console.log(JSON.stringify({ task_id: task, task_pass: false, decision_survived: false, checks: {}, application_errors: [String(error.stack || error)] })); });
