import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import vm from 'node:vm';

// A bounded execution context, not a security boundary for untrusted code.
try {
  const { entry, input } = JSON.parse(readFileSync(0, 'utf8'));
  let source = readFileSync(entry, 'utf8');
  if (entry.endsWith('.ts')) source = stripTypeScriptTypes(source);
  const context = vm.createContext({}, { codeGeneration: { strings: false, wasm: false } });
  // Build objects inside the context rather than exposing host objects.
  vm.runInContext(`globalThis.module = { exports: {} }; globalThis.ctx = ${JSON.stringify(input)};`, context, { timeout: 500 });
  vm.runInContext(source, context, { timeout: 500, filename: entry });
  const response = vm.runInContext(`
    if (typeof module.exports !== 'function') throw new Error('Export a function with module.exports');
    const result = module.exports(Object.freeze(ctx));
    if (result && typeof result.then === 'function') throw new Error('Async plugins are not supported');
    JSON.stringify(result ?? {});
  `, context, { timeout: 500 });
  process.stdout.write(response);
} catch (error) {
  process.stderr.write(String(error?.message ?? error));
  process.exitCode = 1;
}
