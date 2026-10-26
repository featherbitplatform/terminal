import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'featherbit terminal',
  tagline: 'A native terminal for people who live in many SSH sessions.',
  favicon: 'img/featherbit-favicon.png',

  future: {
    v4: true,
  },

  url: 'https://featherbitplatform.github.io',
  baseUrl: '/terminal/',
  organizationName: 'featherbitplatform',
  projectName: 'terminal',
  trailingSlash: false,

  onBrokenLinks: 'throw',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          routeBasePath: '/',
          sidebarPath: './sidebars.ts',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/featherbit-mark.png',
    colorMode: {
      defaultMode: 'dark',
      respectPrefersColorScheme: false,
    },
    navbar: {
      title: 'featherbit terminal',
      logo: {
        alt: 'featherbit logo',
        src: 'img/featherbit-mark.png',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docs',
          position: 'left',
          label: 'Docs',
        },
        {
          href: 'https://github.com/featherbitplatform/terminal',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [],
      copyright: `Apache-2.0 License · featherbit ${new Date().getFullYear()}`,
    },
    prism: {
      theme: prismThemes.oneLight,
      darkTheme: prismThemes.oneDark,
      additionalLanguages: ['toml', 'bash'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
