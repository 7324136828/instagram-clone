/// <reference types="node" />
/**
 * What README.md and instagram.md say the launchers print and which address to open has to match
 * the launchers (run.py for development, backend/packaged.py for a portable build) and the
 * project's port_mapping.json.
 */
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import readme from '../../README.md?raw';
import runbook from '../../instagram.md?raw';

const project = (path: string): string => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
const packaged = project('backend/packaged.py');
const runner = project('run.py');
const mapping = JSON.parse(project('port_mapping.json')) as { frontend_port: number; backend_port: number };

/** The ```text blocks of the README whose lines start with the launcher banner. */
function startupSamples(): string[][] {
  return [...readme.replace(/\r\n/g, '\n').matchAll(/```text\n([\s\S]*?)\n```/g)]
    .map((match) => match[1].split('\n'))
    .filter((lines) => lines.includes('Starting Instagram Clone Full-Stack Services'));
}

/** The "Label:   " prefix of a sample line, padded the way the launchers pad it. */
const label = (line: string): string | undefined => /^([A-Z][A-Za-z ]*: +)/.exec(line)?.[1];

describe('the startup output shown in the README', () => {
  const [development, portable] = startupSamples();

  it('has one sample for run.bat and one for the portable build', () => {
    expect(startupSamples()).toHaveLength(2);
    expect(development).not.toContain('Role:     all');
    expect(portable).toContain('Role:     all');
  });

  it('only shows lines the launcher really prints, and every one it prints', () => {
    const cases: Array<[string[], string, string[]]> = [
      [development, runner, ['Python:   ', 'Backend:  ', 'Frontend: ', 'LAN:      ', 'LAN API:  ']],
      [portable, packaged, ['Role:     ', 'Python:   ', 'Backend:  ', 'Frontend: ', 'LAN:      ', 'LAN API:  ', 'Settings: ', 'Database: ', 'Cache:    ']],
    ];
    for (const [sample, source, labels] of cases) {
      const shown = sample.map(label).filter((value): value is string => value !== undefined);
      expect(new Set(shown)).toEqual(new Set(labels));
      for (const prefix of labels) expect(source, prefix).toContain(`f"${prefix}{`);
    }
    expect(runner).toContain('print("Press Ctrl+C to stop both services.")');
    expect(development).toContain('Press Ctrl+C to stop both services.');
    expect(packaged).toContain('print("[run] Press Ctrl+C to stop.", flush=True)');
    expect(portable).toContain('[run] Press Ctrl+C to stop.');
  });

  it('shows the frontend address the way each launcher writes it', () => {
    // run.py names the local frontend "localhost"; the portable server the loopback address.
    expect(development).toContain('Frontend: http://localhost:5173');
    expect(packaged).toMatch(/if host == "0\.0\.0\.0":\s+host = "127\.0\.0\.1"/);
    expect(portable.find((line) => line.startsWith('Frontend: '))).toMatch(/^Frontend: http:\/\/127\.0\.0\.1:\d+$/);
  });
});

describe('the default ports the documentation names', () => {
  it('are the ones in port_mapping.json', () => {
    expect(readme).toContain(`(currently frontend \`${mapping.frontend_port}\` and backend \`${mapping.backend_port}\`)`);
    expect(readme).toContain(`by default \`${mapping.frontend_port}\` and \`${mapping.backend_port}\``);
  });

  it('tell the runbook reader which address to open for each launcher', () => {
    const text = runbook.replace(/\r\n/g, '\n');
    const start = text.indexOf('\n## 11 Importing into the clone');
    const step = text.slice(start, text.indexOf('\n2. ', start));
    expect(step).toContain('`http://localhost:5173` for `run.bat`');
    expect(step).toContain(`\`http://127.0.0.1:${mapping.frontend_port}\` for a portable build`);
  });

  it('come with the warning that a saved settings.json wins over port_mapping.json', () => {
    const text = readme.replace(/\s+/g, ' ');
    expect(text).toContain('editing `port_mapping.json` changes nothing there');
    expect(text).toContain('on a machine that has already run the app, change `settings.json` instead');
  });
});
