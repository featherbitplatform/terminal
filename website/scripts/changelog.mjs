// Copies the repository's CHANGELOG.md into docs/ so the docs site has a Changelog page.
// Runs before `npm start` and `npm run build` (see package.json); docs/changelog.md is
// git-ignored. CHANGELOG.md stays the only place to edit.
import {copyFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

const website = dirname(dirname(fileURLToPath(import.meta.url)));
copyFileSync(join(website, '..', 'CHANGELOG.md'), join(website, 'docs', 'changelog.md'));
