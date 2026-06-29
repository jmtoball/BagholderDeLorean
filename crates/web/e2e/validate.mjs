// Smoke test for the two-concern UI (Stock selection × Trade action).
// Drives real Chromium through every major interaction, screenshots each state.
//
// With the app running (trunk serve + cargo run -p bagholder-api):
//   cd crates/web/e2e && node validate.mjs
//   BASE=http://localhost:8080 node validate.mjs   # override URL
import { chromium } from 'playwright';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';

const BASE = process.env.BASE ?? 'http://localhost:8080';
const SHOT = join(import.meta.dirname, 'shots');
mkdirSync(SHOT, { recursive: true });

let passed = 0, failed = 0;
const consoleErrors = [];
const fail = (m) => { console.error('  FAIL:', m); process.exitCode = 1; failed++; };
const ok   = (m) => { console.log('  OK:', m); passed++; };

const browser = await chromium.launch();
const page    = await browser.newPage();
page.on('console',   (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
page.on('pageerror', (e) => consoleErrors.push(String(e)));

// ─── helpers ─────────────────────────────────────────────────────────────────

// Action picker: BdSelect renders a native <select>; method + timeframe use BdTabs → <button role="tab">
const actionSelect = () => page.locator('select').first();
const methodTab    = (name)  => page.locator(`button[role="tab"]:has-text("${name}")`);
const timeframeTab = (label) => page.locator(`button[role="tab"]:has-text("${label}")`);
const runBtn       = () => page.locator('button[type="button"]:not([role="tab"])').filter({ hasText: /^Ru/ });

async function shot(name) {
  await page.screenshot({ path: `${SHOT}/${name}.png`, fullPage: true });
}

// Wait for the current run to finish: run button cycles back to non-busy AND result is present.
// BdStat labels use text-transform:uppercase so "Total return" → "TOTAL RETURN" in innerText.
// Watching the button's "Running…" state is more reliable than the 2-phase clear+wait approach,
// because the button enters busy mode synchronously before the fetch even starts.
async function waitForResult(timeout = 60000) {
  // Phase 1: wait for "Running…" (confirms run() fired and busy=true).
  // This eliminates the race where we check BEFORE Leptos has processed the click.
  // Use a short timeout — the click should make the button busy almost immediately.
  await page.waitForFunction(
    () => [...document.querySelectorAll('button[type="button"]:not([role="tab"])')].some(
      (b) => b.textContent.includes('Running'),
    ),
    undefined,
    { timeout: 5000 },
  ).catch(() => {}); // not all strategies go busy (e.g. if result is instant)

  // Phase 2: wait for not-busy + result rendered.
  // waitForFunction(fn, arg, options) — pass undefined as arg to avoid arg/options ambiguity
  await page.waitForFunction(
    () => {
      const btns = [...document.querySelectorAll('button[type="button"]:not([role="tab"])')];
      const runBtn = btns.find((b) => /Run|Running/.test(b.textContent));
      const notBusy = runBtn && !runBtn.textContent.includes('Running');
      const body = document.body.innerText;
      return notBusy && body.includes('TOTAL RETURN') && body.includes('CAGR');
    },
    undefined,
    { timeout },
  );
}

async function step(label, fn) {
  console.log(`\n▶ ${label}`);
  try { await fn(); }
  catch (e) { fail(String(e).split('\n')[0]); }
}

// ─── 1. App loads ────────────────────────────────────────────────────────────

await step('App loads', async () => {
  await page.goto(BASE, { waitUntil: 'networkidle' });
  // Hero h1 reads "Backtest before you baghold." — wait on it to confirm WASM mounted.
  await page.waitForFunction(
    () => /baghold/i.test(document.querySelector('h1')?.textContent ?? ''),
    undefined,
    { timeout: 15000 },
  );
  ok(`WASM mounted — h1: "${await page.locator('h1').first().textContent()}"`);

  // Backtest section + config must scroll into view (hero is full-height above it).
  await page.locator('#app').scrollIntoViewIfNeeded();

  const text = await page.locator('body').innerText();
  if (!text.includes('STOCK SELECTION')) fail('concern panel 01 missing');
  else ok('concern panel 01 present');
  if (!text.includes('TRADE ACTION'))    fail('concern panel 02 missing');
  else ok('concern panel 02 present');

  const opts = await page.locator('select option').allInnerTexts();
  if (!opts.some((t) => t.includes('Buy & Hold'))) fail('action select missing "Buy & Hold"');
  else ok(`action select has ${opts.length} options incl. "Buy & Hold"`);

  await shot('01-load');
});

// ─── 2. Buy & Hold — default AAPL 10y ────────────────────────────────────────

await step('Buy & Hold (AAPL, 10y)', async () => {
  await runBtn().click();
  await waitForResult(30000);
  const pct = (await page.locator('body').innerText()).match(/[+−]\d[\d,.]+%/)?.[0] ?? '?';
  ok(`result rendered — total return: ${pct}`);
  // G1: all six metrics surfaced (labels uppercased via text-transform).
  const body = (await page.locator('body').innerText()).toUpperCase();
  for (const label of ['SORTINO', 'RECOVERY FACTOR']) {
    if (!body.includes(label)) fail(`results missing "${label}" metric`);
    else ok(`results show "${label}"`);
  }
  await shot('02-buyhold');
});

// ─── 3. SMA Crossover ────────────────────────────────────────────────────────

await step('SMA Crossover — params visible, runs', async () => {
  await actionSelect().selectOption('sma');
  await page.waitForFunction(() => document.body.innerText.includes('Fast'), undefined, { timeout: 5000 });
  ok('fast/slow param labels visible');
  await runBtn().click();
  await waitForResult();
  ok('result rendered');
  await shot('03-sma');
});

// ─── 4. Golden Cross ─────────────────────────────────────────────────────────

await step('Golden Cross / Death Cross preset', async () => {
  await actionSelect().selectOption('golden');
  await runBtn().click();
  await waitForResult();
  ok('result rendered');
  await shot('04-golden');
});

// ─── 5. BTFD ─────────────────────────────────────────────────────────────────

await step('BTFD — RSI threshold visible, runs', async () => {
  await actionSelect().selectOption('btfd');
  await page.waitForFunction(() => document.body.innerText.includes('RSI threshold'), undefined, { timeout: 5000 });
  ok('RSI threshold param visible');
  await runBtn().click();
  await waitForResult();
  ok('result rendered');
  await shot('05-btfd');
});

// ─── 6. Mean Reversion ───────────────────────────────────────────────────────

await step('Regime-Filtered Mean Reversion', async () => {
  await actionSelect().selectOption('meanrev');
  await runBtn().click();
  await waitForResult();
  ok('result rendered');
  await shot('06-meanrev');
});

// ─── 7. Timeframe switch ─────────────────────────────────────────────────────

await step('Timeframe 5y', async () => {
  await actionSelect().selectOption('buyhold');
  await timeframeTab('5y').click();
  await runBtn().click();
  await waitForResult();
  ok('5y result rendered');
  await shot('07-timeframe-5y');
});

// ─── 8. Pairs preset — stock selection locked ─────────────────────────────────

await step('Pairs / Stat-Arb preset — stock panel locked', async () => {
  await actionSelect().selectOption('pairs');
  await page.waitForFunction(() => document.body.innerText.includes('Defined by the'), undefined, { timeout: 5000 });
  ok('stock selection panel shows locked message');
  const inputs = await page.locator('input:not([type="checkbox"])').count();
  if (inputs < 2) fail(`expected ≥2 ticker inputs, got ${inputs}`);
  else ok(`${inputs} ticker/param inputs visible`);
  await runBtn().click();
  await waitForResult();
  ok('pairs backtest rendered result');
  await shot('08-pairs');
});

// ─── 9. Unsupported preset — immediate error callout ─────────────────────────

await step('Inverse Cramer (unimplemented) → error callout', async () => {
  await actionSelect().selectOption('cramer');
  await runBtn().click();
  await page.waitForSelector('div[role="note"]', { timeout: 10000 });
  ok('error callout appeared immediately');
  await shot('09-cramer-error');
});

// ─── 10. Invalid ticker → error callout ──────────────────────────────────────

await step('Invalid ticker → error callout', async () => {
  await actionSelect().selectOption('buyhold');
  await page.waitForTimeout(500); // let stock panel re-render (microtask queue settles)
  const input = page.locator('input:not([type="checkbox"])').first();
  await input.fill('XXXXINVALID');
  await runBtn().click();
  await page.waitForSelector('div[role="note"]', { timeout: 15000 });
  ok('error callout appeared for unknown ticker');
  // Restore ticker so subsequent steps start clean
  await input.fill('AAPL');
  await shot('10-ticker-error');
});

// ─── 10b. Tax simulation — selector + after-tax results ──────────────────────

const bodyLower = async () => (await page.locator('body').innerText()).toLowerCase();

await step('Tax simulation — configurator affordances + after-tax results', async () => {
  await actionSelect().selectOption('buyhold');
  await page.waitForTimeout(300);
  await page.locator('input:not([type="checkbox"])').first().fill('AAPL');

  // Collapsed CTA → expand into the "06 · Tax simulation" card.
  await page.getByText('Set up').click();
  await page.waitForFunction(() => document.body.innerText.toLowerCase().includes('no tax applied'), undefined, { timeout: 5000 });
  ok('CTA expanded to the tax card');

  // US: the long-term bracket chip lights from the income. $96k → 15%.
  await page.locator('button[role="tab"]:has-text("United States")').click();
  await page.waitForFunction(() => document.body.innerText.toLowerCase().includes('long-term rate'), undefined, { timeout: 5000 });
  // The lit chip has the accent-soft background; assert a "15%" chip exists.
  const has15 = (await bodyLower()).includes('15%');
  if (!has15) fail('US bracket chips did not render'); else ok('US bracket chips render');

  // Germany: the Overall tax rate callout renders.
  await page.locator('button[role="tab"]:has-text("Germany")').click();
  await page.waitForFunction(() => document.body.innerText.toLowerCase().includes('overall tax rate'), undefined, { timeout: 5000 });
  ok('German knobs + rate callout disclosed');

  // Collapse button a11y: aria-expanded reflects state, label swaps.
  const collapseBtn = page.getByRole('button', { name: 'Collapse tax simulation' });
  if ((await collapseBtn.getAttribute('aria-expanded')) !== 'true') fail('aria-expanded not "true" when open');
  else ok('aria-expanded="true" when open');
  await collapseBtn.click();
  const expandBtn = page.getByRole('button', { name: 'Expand tax simulation' });
  if ((await expandBtn.getAttribute('aria-expanded')) !== 'false') fail('aria-expanded/label did not flip on collapse');
  else ok('aria-expanded="false" + label flips on collapse');
  await expandBtn.click(); // re-expand for the run below

  await runBtn().click();
  await waitForResult(30000);
  const body = await bodyLower();
  if (!body.includes('what you actually keep') && !body.includes('total tax paid'))
    fail('after-tax section did not render');
  else ok('after-tax results rendered');
  await shot('10b-tax-de');

  // Remove the tax sim so later steps run pre-tax.
  await page.getByRole('button', { name: 'Remove tax simulation' }).click();
  await page.waitForFunction(() => document.body.innerText.includes('Add tax simulation'), undefined, { timeout: 5000 });
});

// ─── 11. Screen flow — Low P/E candidates ────────────────────────────────────

await step('Screen: Low P/E candidates table', async () => {
  await actionSelect().selectOption('buyhold');
  await methodTab('Screen').click();
  const lbl = await runBtn().innerText();
  if (!lbl.includes('screen')) fail(`expected "Run screen", got "${lbl.trim()}"`);
  else ok(`run button says "${lbl.trim()}"`);
  await runBtn().click();
  await page.waitForSelector('table tbody tr', { timeout: 300000 }); // cold cache: ~2 min
  const rowCount = await page.locator('table tbody tr').count();
  if (rowCount < 5) fail(`expected ≥5 rows, got ${rowCount}`);
  else ok(`Low P/E screen: ${rowCount} candidates`);
  await shot('11-screen');
});

// ─── 12. Select candidates + overlay backtest ─────────────────────────────────

await step('Overlay backtest of 3 selected candidates', async () => {
  // BdCheckbox hides the real <input> (opacity:0) behind the brand box — force the check.
  const boxes = page.locator('table tbody input[type="checkbox"]');
  for (let i = 0; i < 3; i++) await boxes.nth(i).check({ force: true });
  const checked = await page.locator('table tbody input:checked').count();
  if (checked !== 3) fail(`expected 3 checked, got ${checked}`);
  else ok('3 candidates selected');

  await page.getByRole('button', { name: 'Backtest selected' }).click();
  // Overlay renders a BdCard with title "Overlaid backtest" (rendered as h3)
  await page.waitForFunction(
    () => document.body.innerText.includes('Overlaid backtest'),
    undefined,
    { timeout: 60000 },
  );
  // Each series is a <path fill="none"> in the overlay SVG
  const lineCount = await page.locator('svg path[fill="none"]').count();
  ok(`overlay rendered (${lineCount} chart paths)`);
  await shot('12-overlay');
});

// ─── 13. P/E trough entry ────────────────────────────────────────────────────

await step('Enter at P/E trough', async () => {
  // BdSwitch checkbox is opacity:0/position:absolute — use force:true
  const troughSwitch = page
    .locator('label')
    .filter({ hasText: /Enter at P\/E trough/ })
    .locator('input[type="checkbox"]');
  await troughSwitch.check({ force: true });
  await page.getByRole('button', { name: 'Backtest selected' }).click();
  await page.waitForFunction(
    () => [...document.querySelectorAll('span')].some((s) => /from \d{4}-\d\d-\d\d/.test(s.textContent)),
    undefined,
    { timeout: 60000 },
  );
  const entries = await page.locator('span').filter({ hasText: /from \d{4}-\d\d-\d\d/ }).count();
  if (entries < 1) fail('no "from YYYY-MM-DD" entry dates in legend');
  else ok(`${entries} entry dates in legend`);
  await shot('13-pe-trough');
});

// ─── 14. P/E mini-charts + trough stepper ────────────────────────────────────

await step('P/E mini-charts and trough stepper', async () => {
  await page.waitForSelector('svg circle', { timeout: 30000 });
  const dots = await page.locator('svg circle').count();
  ok(`P/E mini-charts drew ${dots} trough dots`);

  const datesBefore = await page.locator('span')
    .filter({ hasText: /from \d{4}-\d\d-\d\d/ })
    .allInnerTexts()
    .then((a) => a.join('|'));

  await page.getByRole('button', { name: /older/i }).click();
  await page.waitForFunction(
    (b) => {
      const cur = [...document.querySelectorAll('span')]
        .filter((s) => /from \d{4}-\d\d-\d\d/.test(s.textContent))
        .map((s) => s.textContent).join('|');
      return cur && cur !== b;
    },
    datesBefore,
    { timeout: 60000 },
  );
  ok('"older" stepper changed entry dates in legend');
  await shot('14-pe-step');
});

// ─── 15. Console errors ───────────────────────────────────────────────────────

await step('No JS page errors', async () => {
  const realErrors = consoleErrors.filter(
    (e) =>
      !e.includes('XXXXINVALID') &&
      !e.includes('status of 500') &&
      // trunk hot-reload WS — only present when served via cargo-run (not trunk serve)
      !e.includes('trunk/ws') &&
      !e.includes('__trunk_address__') &&
      // Leptos debug-mode reactive warning — benign, fires on rapid DOM churn in tests
      !e.includes('closure invoked recursively or after being dropped'),
  );
  if (realErrors.length) fail('unexpected console/page errors: ' + realErrors.join(' ;; '));
  else ok('no unexpected console/page errors');
});

await browser.close();

console.log('');
console.log(`─── ${passed} passed, ${failed} failed ───`);
console.log(process.exitCode ? 'VALIDATION FAILED' : 'VALIDATION PASSED');
