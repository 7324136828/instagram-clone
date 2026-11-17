/**
 * A build bundles third-party packages, one of them (heic-to) under the LGPL, so their licence
 * texts are shipped with it: Vite copies frontend/public/licenses into every build. This keeps
 * that folder honest when a dependency is added or upgraded.
 */
import { describe, expect, it } from 'vitest';
import fflateLicense from '../node_modules/fflate/LICENSE?raw';
import heicToLicense from '../node_modules/heic-to/LICENSE?raw';
import heicToPackage from '../node_modules/heic-to/package.json?raw';
import reactDomLicense from '../node_modules/react-dom/LICENSE?raw';
import reactLicense from '../node_modules/react/LICENSE?raw';
import schedulerLicense from '../node_modules/scheduler/LICENSE?raw';
import packageJson from '../package.json?raw';

const shipped = Object.fromEntries(
  Object.entries(
    import.meta.glob<string>('../public/licenses/*', { query: '?raw', import: 'default', eager: true }),
  ).map(([path, text]) => [path.slice(path.lastIndexOf('/') + 1), text]),
);

/** The LICENSE file each bundled package is installed with. */
const installed: Record<string, string> = {
  'heic-to': heicToLicense,
  fflate: fflateLicense,
  react: reactLicense,
  'react-dom': reactDomLicense,
  // Not a direct dependency: react-dom brings it along and it ends up in the bundle.
  scheduler: schedulerLicense,
};

describe('licence texts shipped with a build', () => {
  const dependencies = Object.keys((JSON.parse(packageJson) as { dependencies: Record<string, string> }).dependencies);

  it('cover every runtime dependency', () => {
    // A new dependency needs its licence in public/licenses, an entry above and a line in README.txt.
    for (const name of dependencies) expect(Object.keys(installed), `no licence is shipped for "${name}"`).toContain(name);
  });

  // A checkout may convert line ends; the words are what has to be the same.
  const lines = (text: string | undefined) => text?.replace(/\r\n/g, '\n');

  it.each(Object.keys(installed))('%s: the shipped text is the one the installed package comes with', (name) => {
    expect(lines(shipped[`${name}.LICENSE.txt`])).toBe(lines(installed[name]));
    expect(shipped['README.txt']).toContain(`${name}.LICENSE.txt`);
  });

  it('include both texts the LGPL library needs', () => {
    expect((JSON.parse(heicToPackage) as { license: string }).license).toBe('LGPL-3.0');
    expect(shipped['heic-to.LICENSE.txt']).toContain('GNU LESSER GENERAL PUBLIC LICENSE');
    expect(shipped['heic-to.LICENSE.txt']).toContain('Version 3, 29 June 2007');
    // The LGPL is a set of additional permissions on top of the GPL, whose text it does not repeat.
    expect(shipped['GPL-3.0.txt']).toMatch(/^\s*GNU GENERAL PUBLIC LICENSE\s+Version 3, 29 June 2007/);
    expect(shipped['GPL-3.0.txt']).toContain('END OF TERMS AND CONDITIONS');
    expect(shipped['README.txt']).toContain('GPL-3.0.txt');
  });

  it('contain nothing that the index does not explain', () => {
    for (const file of Object.keys(shipped)) {
      if (file !== 'README.txt') expect(shipped['README.txt'], file).toContain(file);
    }
  });
});
