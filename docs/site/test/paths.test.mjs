import assert from 'node:assert/strict';
import path from 'node:path';
import test from 'node:test';
import { docsRoot, documentId, documentLinks, isDocumentation, rewriteLink, repository } from '../src/lib/paths.mjs';
import remarkDocs from '../src/lib/remark-docs.mjs';

const source = path.join(docsRoot, 'architecture/diagrams.md');

test('sidebar groups include pages loaded outside Starlight’s default content directory', () => {
  assert.ok(documentLinks('architecture').some(({ slug }) => slug === 'architecture/runtime'));
  assert.ok(documentLinks('protocol').some(({ slug }) => slug === 'protocol/index'));
  assert.ok(documentLinks('spec').some(({ slug }) => slug === 'spec/backend/index'));
});

test('README routes and links work at the root and under a Pages base path', () => {
  assert.equal(documentId('README.md'), 'index');
  assert.equal(documentId('protocol/README.md'), 'protocol/index');
  assert.equal(rewriteLink('../README.md', source), '/');
  assert.equal(rewriteLink('../README.md', source, '/hya'), '/hya/');
  assert.equal(rewriteLink('../protocol/README.md#routes', source, '/hya/'), '/hya/protocol/#routes');
  assert.equal(rewriteLink('../cli.md?view=all#exec', source, '/hya'), '/hya/cli/?view=all#exec');
  assert.equal(rewriteLink('overview.md', source, '/hya'), '/hya/architecture/overview/');
});

test('assets and the OpenAPI schema remain downloadable on subpath deployments', () => {
  assert.equal(rewriteLink('../assets/hya-architecture.svg', source, '/hya'), '/hya/hya-architecture.svg');
  assert.equal(rewriteLink('../protocol/openapi.json', source, '/hya'), '/hya/protocol/openapi.json');
});

test('repository files, examples, and archived notes link to their GitHub sources', () => {
  assert.equal(rewriteLink('../../Cargo.toml', source, '/hya'), `${repository}/blob/main/Cargo.toml`);
  assert.equal(rewriteLink('../examples/', source, '/hya'), `${repository}/tree/main/docs/examples`);
  assert.equal(rewriteLink('../examples/bundle.hya.md', source), `${repository}/blob/main/docs/examples/bundle.hya.md`);
  assert.equal(isDocumentation('changes/CHANGELOG_0.47.0.md'), false);
  assert.equal(isDocumentation('site/node_modules/example.md'), false);
});

test('external URLs and fragment-only links are preserved', () => {
  for (const url of ['https://example.com/a.md', 'mailto:dev@example.com', '//example.com', '#routes']) {
    assert.equal(rewriteLink(url, source, '/hya'), url);
  }
});

test('Markdown adaptation preserves code, rewrites references, and escapes Mermaid source', () => {
  const tree = { type: 'root', children: [
    { type: 'heading', depth: 1, children: [{ type: 'text', value: 'Diagrams' }] },
    { type: 'definition', identifier: 'guide', url: '../cli.md' },
    { type: 'code', lang: 'sh', value: 'cat ../cli.md' },
    { type: 'code', lang: 'mermaid', value: 'A["<script>&"] --> B' },
  ] };
  remarkDocs({ base: '/hya' })(tree, { path: source });
  assert.equal(tree.children.length, 3);
  assert.equal(tree.children[0].url, '/hya/cli/');
  assert.equal(tree.children[1].value, 'cat ../cli.md');
  assert.equal(tree.children[2].type, 'html');
  assert.match(tree.children[2].value, /&lt;script&gt;&amp;/);
});
