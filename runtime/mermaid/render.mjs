import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const runtime = dirname(fileURLToPath(import.meta.url));
process.env.PLAYWRIGHT_BROWSERS_PATH ??= resolve(runtime, '.browsers');
const { chromium } = await import('playwright');
let browser;
try {
  const { source, output } = JSON.parse(readFileSync(0, 'utf8'));
  browser = await chromium.launch({ headless: true, executablePath: process.env.REDITOR_CHROMIUM || undefined });
  const page = await browser.newPage({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1 });
  await page.route('**/*', route => route.abort()); // diagrams render entirely offline
  await page.setContent('<!doctype html><html><head><meta charset="UTF-8"><style>body{margin:0;background:#111722}svg{display:block;max-width:none!important}#diagram{display:inline-block;padding:12px}</style></head><body><div id="diagram"></div></body></html>');
  await page.addScriptTag({ path: resolve(runtime, '../../assets/mermaid.min.js') });
  const rendered = await page.evaluate(async source => {
    const mermaid = globalThis.ReditorMermaid.default;
    mermaid.initialize({ startOnLoad: false, theme: 'dark', securityLevel: 'strict', suppressErrorRendering: true, maxTextSize: 100000, flowchart: { htmlLabels: false }, fontFamily: 'Arial, sans-serif' });
    const result = await mermaid.render('reditor-diagram', source);
    document.getElementById('diagram').innerHTML = result.svg;
    const svg = document.querySelector('#diagram svg');
    const viewBox = svg.viewBox.baseVal;
    const width = Math.max(1, Math.ceil(viewBox.width));
    const height = Math.max(1, Math.ceil(viewBox.height));
    if (width * height > 16000000 || width > 8192 || height > 8192) throw new Error('Diagram exceeds the rendering size limit');
    svg.setAttribute('width', width); svg.setAttribute('height', height);
    svg.style.width = `${width}px`; svg.style.height = `${height}px`;
    // Preserve real text and vector geometry for ordinary text terminals.
    const origin = svg.getBoundingClientRect();
    const labels = [];
    const visitText = element => {
      if (element.querySelector('tspan')) {
        for (const child of element.children) visitText(child);
        return;
      }
      const text = element.textContent.trim();
      const box = element.getBoundingClientRect();
      if (text && box.width && box.height) labels.push({ text, x: box.x - origin.x, y: box.y - origin.y, width: box.width, height: box.height });
    };
    for (const text of svg.querySelectorAll('text')) visitText(text);
    // Some Mermaid families use HTML labels even when flowchart htmlLabels is false.
    for (const element of svg.querySelectorAll('foreignObject')) {
      const box = element.getBoundingClientRect();
      const lines = element.innerText?.split('\n') ?? element.textContent.split('\n');
      const content = lines.map(line => line.trim()).filter(Boolean);
      content.forEach((text, index) => labels.push({ text, x: box.x - origin.x, y: box.y - origin.y + index * box.height / content.length, width: box.width, height: box.height / content.length }));
    }
    const paths = [];
    let pointCount = 0;
    for (const element of svg.querySelectorAll('path,rect,circle,ellipse,line,polygon,polyline')) {
      if (element.closest('defs,clipPath,marker') || !element.getTotalLength) continue;
      const box = element.getBoundingClientRect();
      if (!box.width && !box.height) continue;
      const length = element.getTotalLength();
      if (!Number.isFinite(length) || !length) continue;
      const count = Math.min(2048, Math.max(2, Math.ceil(length / 4)));
      pointCount += count + 1;
      if (pointCount > 200000) throw new Error('Diagram exceeds the vector complexity limit');
      const matrix = element.getScreenCTM();
      const points = Array.from({ length: count + 1 }, (_, index) => {
        const point = element.getPointAtLength(length * index / count).matrixTransform(matrix);
        return [point.x - origin.x, point.y - origin.y];
      });
      paths.push(points);
      if (element.getAttribute('marker-end') && count > 1) {
        const end = points.at(-1);
        const previous = points.at(-2);
        const angle = Math.atan2(end[1] - previous[1], end[0] - previous[0]);
        const side = delta => [end[0] - 9 * Math.cos(angle + delta), end[1] - 9 * Math.sin(angle + delta)];
        paths.push([side(0.5), end, side(-0.5)]);
      }
    }
    return { svg: result.svg, geometry: { width, height, labels, paths } };
  }, source);
  writeFileSync(resolve(output, 'diagram.svg'), rendered.svg);
  writeFileSync(resolve(output, 'diagram.json'), JSON.stringify(rendered.geometry));
  await page.locator('#diagram').screenshot({ path: resolve(output, 'diagram.png'), timeout: 15000 });
  process.stdout.write(JSON.stringify({ ok: true }));
} catch (error) {
  process.stderr.write(String(error?.message ?? error));
  process.exitCode = 1;
} finally {
  if (browser) await browser.close();
}
