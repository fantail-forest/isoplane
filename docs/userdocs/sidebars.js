// @ts-check

/**
 * One sidebar, four sections, in Diátaxis order: learning, doing, looking up,
 * understanding. The order is deliberate — a reader who does not yet know
 * what WISP is should meet the tutorials first, and a reader who does should
 * be able to skip straight to reference without reading prose.
 *
 * @type {import('@docusaurus/plugin-content-docs').SidebarsConfig}
 */
const sidebars = {
  docs: [
    'index',
    {
      type: 'category',
      label: 'Tutorials',
      collapsed: false,
      link: {type: 'doc', id: 'tutorials/index'},
      items: [
        'tutorials/quickstart',
        'tutorials/first-csharp-guest',
        'tutorials/first-rust-guest',
      ],
    },
    {
      type: 'category',
      label: 'How-to guides',
      collapsed: false,
      link: {type: 'doc', id: 'how-to/index'},
      items: [
        'how-to/use-the-sandbox',
        'how-to/build-a-csharp-guest',
        'how-to/debug-a-rejected-module',
        'how-to/observe-a-running-node',
      ],
    },
    {
      type: 'category',
      label: 'Reference',
      collapsed: false,
      link: {type: 'doc', id: 'reference/index'},
      items: [
        'reference/architecture',
        'reference/wisp-toml',
        'reference/cli-wisp',
        'reference/cli-other',
        'reference/guest-abi',
        'reference/capabilities',
        'reference/sdk-csharp',
        'reference/sdk-rust',
        'reference/metrics',
        'reference/validator',
      ],
    },
    {
      type: 'category',
      label: 'Explanation',
      collapsed: false,
      link: {type: 'doc', id: 'explanation/index'},
      items: [
        'explanation/invariants',
        'explanation/lifecycle',
        'explanation/deploy-pipeline',
      ],
    },
  ],
};

export default sidebars;
