import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import { resolveDocsBase, resolvePagesBase } from '../src/lib/paths.mjs';

const pagesOutput = path.resolve('dist-pages');
const docsOutput = path.join(pagesOutput, 'docs');
const pagesBase = resolvePagesBase();
const docsBase = resolveDocsBase().replace(/\/$/, '');

assert.ok(existsSync(path.join(pagesOutput, 'index.html')), 'Missing root landing page dist-pages/index.html');

// 1. Verify root landing page local asset & docs entry links
const landingHtml = readFileSync(path.join(pagesOutput, 'index.html'), 'utf8');
const brokenLanding = [];
for (const [, rawUrl] of landingHtml.matchAll(/(?:href|src)="([^"#?]+)(?:[?#][^"]*)?"/g)) {
  if (/^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(rawUrl)) continue;
  const rel = rawUrl.startsWith('/') ? rawUrl.slice(pagesBase.length).replace(/^\//, '') : rawUrl.replace(/^\.\//, '');
  const target = path.join(pagesOutput, decodeURI(rel));
  const exists = existsSync(target) && (statSync(target).isFile() || existsSync(path.join(target, 'index.html')));
  if (!exists) brokenLanding.push(`index.html: ${rawUrl}`);
}
assert.deepEqual(brokenLanding, [], `Broken root landing page links:\n${brokenLanding.join('\n')}`);
assert.ok(landingHtml.includes('href="docs/"'), 'Root landing page must link to docs/');

// 2. Verify Starlight documentation pages, navigation, assets, OpenAPI, and Pagefind
const htmlFiles = readdirSync(docsOutput, { recursive: true }).filter((file) => file.endsWith('.html'));
assert.ok(htmlFiles.length >= 65, 'Expected the documentation pages to be rendered');
assert.ok(existsSync(path.join(docsOutput, 'pagefind/pagefind.js')), 'Missing production search index');
JSON.parse(readFileSync(path.join(docsOutput, 'protocol/openapi.json'), 'utf8'));

const brokenDocs = [];
const docsHome = readFileSync(path.join(docsOutput, 'index.html'), 'utf8');
for (const file of htmlFiles) {
  if (file !== '404.html') {
    const route = file.replace(/index\.html$/, '');
    assert.ok(docsHome.includes(`href="${docsBase}/${route}"`), `Page missing from navigation: ${file}`);
  }
  const html = readFileSync(path.join(docsOutput, file), 'utf8');
  for (const [, url] of html.matchAll(/(?:href|src)="([^"#?]+)(?:[?#][^"]*)?"/g)) {
    if (!url.startsWith('/') || url.startsWith('//')) continue;
    assert.ok(url.startsWith(`${docsBase}/`), `${file}: URL escapes docs base path: ${url}`);
    const target = path.join(docsOutput, decodeURI(url.slice(docsBase.length)));
    const exists = existsSync(target) && (statSync(target).isFile() || existsSync(path.join(target, 'index.html')));
    if (!exists) brokenDocs.push(`${file}: ${url}`);
  }
}
assert.deepEqual(brokenDocs, [], `Broken local documentation links:\n${brokenDocs.join('\n')}`);
console.log(`Verified root landing page + ${htmlFiles.length} Starlight HTML pages, local links, assets, OpenAPI, and Pagefind search.`);
