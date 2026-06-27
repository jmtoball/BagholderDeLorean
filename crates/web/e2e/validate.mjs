// In-browser smoke test for the dashboard. Drives a real Chromium through both
// flows (price backtest; fundamentals screen -> select -> overlaid backtests),
// asserts the key DOM appears, and screenshots each state into ./shots.
//
// Setup + run (see README.md): with the app running on :3000,
//   cd crates/web/e2e && npm link playwright && node validate.mjs
// Override the URL with BASE=http://host:port node validate.mjs
import { chromium } from 'playwright';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';

const BASE = process.env.BASE ?? 'http://127.0.0.1:3000';
const SHOT = join(import.meta.dirname, 'shots');
mkdirSync(SHOT, { recursive: true });
const fail = (m) => { console.error('FAIL:', m); process.exitCode = 1; };

const browser = await chromium.launch();
const page = await browser.newPage();
const errors = [];
page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
page.on('pageerror', (e) => errors.push(String(e)));

await page.goto(BASE, { waitUntil: 'networkidle' });
await page.waitForFunction(
  () => document.querySelector('h1')?.textContent?.includes('Bagholder'),
  { timeout: 15000 },
);
console.log('OK: wasm app mounted, h1 =', await page.locator('h1').textContent());

// --- Price flow: default AAPL buy & hold ---
await page.getByRole('button', { name: 'Run backtest' }).click();
await page.waitForSelector('svg polyline', { timeout: 30000 });
const priceMetrics = await page.locator('ul li').allTextContents();
if (!priceMetrics.some((t) => t.includes('Total return'))) fail('price: no Total return metric');
console.log('OK: price backtest ->', priceMetrics.join(' | '));
await page.screenshot({ path: `${SHOT}/01-price.png`, fullPage: true });

// --- Fundamentals flow: screen -> select -> overlaid backtests ---
await page.locator('select').first().selectOption('fundamentals');
await page.getByRole('button', { name: 'Run screen' }).click();
// Cold cache warms ~23 names on the server; allow a few minutes.
await page.waitForSelector('table tbody tr', { timeout: 300000 });
const rowCount = await page.locator('table tbody tr').count();
console.log(`OK: screen returned ${rowCount} candidate rows`);
console.log('   top row:', (await page.locator('table tbody tr').first().innerText()).replace(/\s+/g, ' ').trim());
await page.screenshot({ path: `${SHOT}/02-screen.png`, fullPage: true });

const boxes = page.locator('table tbody tr input[type=checkbox]');
for (let i = 0; i < 3; i++) await boxes.nth(i).check();
const checked = await page.locator('table tbody input:checked').count();
if (checked !== 3) fail(`expected 3 checked, got ${checked}`);

await page.getByRole('button', { name: 'Backtest selected' }).click();
await page.waitForFunction(() => document.querySelectorAll('svg polyline').length >= 3, { timeout: 60000 });
const lineCount = await page.locator('svg polyline').count();
const legend = await page.locator('section span span').count();
console.log(`OK: overlay drew ${lineCount} equity curves, ${legend} legend swatches`);
await page.screenshot({ path: `${SHOT}/03-overlay.png`, fullPage: true });

// --- Local-min P/E entry: legend should show per-name entry dates ---
await page.locator('label', { hasText: 'enter at local-min P/E' }).locator('input').check();
await page.getByRole('button', { name: 'Backtest selected' }).click();
await page.waitForFunction(
  () => [...document.querySelectorAll('span')].some((s) => /from \d{4}-\d\d-\d\d/.test(s.textContent)),
  { timeout: 60000 },
);
const entriesShown = await page.locator('span').filter({ hasText: /from \d{4}-\d\d-\d\d/ }).count();
if (entriesShown < 1) fail('pe_min: no entry dates in legend');
console.log(`OK: local-min entry legend shows ${entriesShown} entry dates`);
await page.screenshot({ path: `${SHOT}/04-pe-min.png`, fullPage: true });

// --- P/E charts: troughs as dots, current entry highlighted red ---
await page.waitForSelector('svg circle', { timeout: 30000 });
const troughDots = await page.locator('svg circle').count();
const entryDots = await page.locator('svg circle[fill="#dc2626"]').count();
if (entryDots < 1) fail('P/E chart: no highlighted entry trough');
console.log(`OK: P/E charts drew ${troughDots} trough dots, ${entryDots} highlighted entries`);
await page.screenshot({ path: `${SHOT}/06-pe-chart.png`, fullPage: true });

// --- Step to an older trough; wait for the legend entry dates to actually
// re-render (the trough label flips on click, before the re-fetch completes,
// so assert on the legend, not the label). ---
const legendDates = () =>
  page.locator('span').filter({ hasText: /from \d{4}-\d\d-\d\d/ }).allInnerTexts().then((a) => a.join('|'));
const datesBefore = await legendDates();
const troughLabel = () => page.locator('span').filter({ hasText: /trough \d+ of \d+/ }).first().innerText();
const labelBefore = await troughLabel();
await page.getByRole('button', { name: /older/ }).click();
await page.waitForFunction(
  (b) => {
    const cur = [...document.querySelectorAll('span')]
      .filter((s) => /from \d{4}-\d\d-\d\d/.test(s.textContent))
      .map((s) => s.textContent)
      .join('|');
    return cur && cur !== b;
  },
  datesBefore,
  { timeout: 60000 },
);
const labelAfter = await troughLabel();
if (labelAfter === labelBefore) fail('stepper: trough label did not advance');
if ((await legendDates()) === datesBefore) fail('stepper: legend entries did not change');
console.log(`OK: stepped "${labelBefore}" -> "${labelAfter}", entries re-rendered`);
await page.screenshot({ path: `${SHOT}/05-step.png`, fullPage: true });

if (errors.length) fail('console/page errors: ' + errors.join(' ;; '));
else console.log('OK: no console/page errors');

await browser.close();
console.log(process.exitCode ? 'VALIDATION FAILED' : 'VALIDATION PASSED');
