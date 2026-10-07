import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

const cases = [
  ['flowchart', 'flowchart LR\n A[Привет] --> B[Rust]', 'Привет'],
  ['sequence', 'sequenceDiagram\n Alice->>Bob: Hello\n Bob-->>Alice: Hi', 'Hello'],
  ['class', 'classDiagram\n Animal <|-- Duck\n Animal : +int age', 'Animal'],
  ['state', 'stateDiagram-v2\n [*] --> Ready\n Ready --> Done', 'Ready'],
  ['pie', 'pie title Languages\n "Rust" : 70\n "JS" : 30', 'Rust'],
];
for (const [name, source, label] of cases) {
  const output = mkdtempSync(resolve(tmpdir(), 'reditor-mermaid-'));
  try {
    const result = spawnSync(process.execPath, ['render.mjs'], { input: JSON.stringify({ source, output }), encoding: 'utf8', timeout: 30000 });
    assert.equal(result.status, 0, `${name}: ${result.stderr}`);
    assert.match(readFileSync(resolve(output, 'diagram.svg'), 'utf8'), new RegExp(label));
    const png = readFileSync(resolve(output, 'diagram.png'));
    assert.equal(png.subarray(1, 4).toString(), 'PNG');
    assert(png.length > 1000);
    const geometry = JSON.parse(readFileSync(resolve(output, 'diagram.json'), 'utf8'));
    assert(geometry.labels.some(item => item.text.includes(label)), `${name}: real terminal label missing`);
    assert(geometry.paths.length > 0, `${name}: outlines missing`);
    assert(geometry.width > 0 && geometry.height > 0);
    assert(geometry.paths.flat().every(point => point.every(Number.isFinite)));
  } finally { rmSync(output, { recursive: true, force: true }); }
}
const output = mkdtempSync(resolve(tmpdir(), 'reditor-mermaid-error-'));
try {
  const result = spawnSync(process.execPath, ['render.mjs'], { input: JSON.stringify({ source: 'not a diagram', output }), encoding: 'utf8', timeout: 30000 });
  assert.notEqual(result.status, 0);
  assert(!existsSync(resolve(output, 'diagram.png')));
} finally { rmSync(output, { recursive: true, force: true }); }
console.log('Mermaid renderer passed: flowchart, sequence, class, state, pie, invalid source, offline SVG/PNG, vector geometry and real text labels');
