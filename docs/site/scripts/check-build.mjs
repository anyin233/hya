import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';

const output = path.resolve('dist');
const base = (process.env.DOCS_BASE || '/').replace(/\/$/, '');
const htmlFiles = readdirSync(output, { recursive: true }).filter((file) => file.endsWith('.html'));
assert.ok(htmlFiles.length >= 65, 'Expected the documentation pages to be rendered');
assert.ok(existsSync(path.join(output, 'pagefind/pagefind.js')), 'Missing production search index');
JSON.parse(readFileSync(path.join(output, 'protocol/openapi.json'), 'utf8'));
const broken = [];
const homepage = readFileSync(path.join(output, 'index.html'), 'utf8');
for (const file of htmlFiles) {
  if (file !== '404.html') {
    const route = file.replace(/index\.html$/, '');
    assert.ok(homepage.includes(`href="${base}/${route}"`), `Page missing from navigation: ${file}`);
  }
  const html = readFileSync(path.join(output, file), 'utf8');
  for (const [, url] of html.matchAll(/(?:href|src)="([^"#?]+)(?:[?#][^"]*)?"/g)) {
    if (!url.startsWith('/') || url.startsWith('//')) continue;
    assert.ok(url.startsWith(`${base}/`), `${file}: URL escapes base path: ${url}`);
    const target = path.join(output, decodeURI(url.slice(base.length)));
    const exists = existsSync(target) && (statSync(target).isFile() || existsSync(path.join(target, 'index.html')));
    if (!exists) broken.push(`${file}: ${url}`);
  }
}
assert.deepEqual(broken, [], `Broken local documentation links:\n${broken.join('\n')}`);
console.log(`Verified ${htmlFiles.length} HTML pages, local links, assets, OpenAPI, and Pagefind search.`);
