import { visit } from 'unist-util-visit';
import { rewriteLink } from './paths.mjs';

export default function remarkDocs({ base = '/' } = {}) {
  return (tree, file) => {
    // Starlight supplies the page H1 from the collection title.
    const heading = tree.children.findIndex((node) => node.type === 'heading' && node.depth === 1);
    if (heading !== -1) tree.children.splice(heading, 1);
    visit(tree, (node) => {
      if (['link', 'image', 'definition'].includes(node.type)) {
        node.url = rewriteLink(node.url, file.path, base);
      }
      if (node.type === 'code' && node.lang === 'mermaid') {
        node.type = 'html';
        node.value = `<pre class="mermaid">${node.value
          .replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')}</pre>`;
      }
    });
  };
}
