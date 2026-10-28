import {visit} from 'unist-util-visit';

// `:since[0.1.3]` marks the thing it follows as added in that release, and
// `:since[next]` as added since the last one; scripts/snapshot.mjs resolves the
// latter when it snapshots the docs for a release. Each becomes the Since
// component. This runs before Docusaurus reads headings, so a marker in one
// stays out of its anchor, its table-of-contents entry and the page title.
export default function since() {
  return (tree, file) => {
    visit(tree, 'textDirective', (node, index, parent) => {
      if (node.name !== 'since') return;
      const version = node.children.map((child) => child.value ?? '').join('').trim();
      if (!version) file.fail('Write a release marker as :since[0.1.3] or :since[next].', node);
      // The marker carries its own spacing. Space before it would otherwise end
      // a heading's anchor with a hyphen.
      const previous = parent.children[index - 1];
      if (previous?.type === 'text') previous.value = previous.value.trimEnd();
      parent.children[index] = {
        type: 'mdxJsxTextElement',
        name: 'Since',
        attributes: [{type: 'mdxJsxAttribute', name: 'version', value: version}],
        children: [],
      };
    });
  };
}
