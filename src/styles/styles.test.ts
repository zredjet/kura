import { describe, expect, it } from 'vitest';

import designTokens from '../../docs/design/tokens.css?raw';
import tokens from './tokens.css?raw';

/** src 配下の CSS（tokens.css 以外）。 */
const stylesheets = Object.entries(
  import.meta.glob<string>('/src/**/*.css', { query: '?raw', import: 'default', eager: true }),
).filter(([path]) => path !== '/src/styles/tokens.css');

const COLOR_LITERALS = [
  /#[0-9a-f]{3,8}\b/i,
  /\b(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch|color)\(/i,
  // white-space などのプロパティ名は除く
  /\b(?:white|black)\b(?!-)/i,
];

function stripComments(css: string): string {
  return css.replace(/\/\*[\s\S]*?\*\//g, '');
}

describe('デザイントークン', () => {
  it('src/styles/tokens.css は docs/design/tokens.css と同じ内容', () => {
    expect(tokens).toBe(designTokens);
  });

  it('CSS を読み込めている', () => {
    expect(stylesheets.length).toBeGreaterThan(0);
  });

  it.each(stylesheets)('%s に色の直書きがない', (_path, css) => {
    const body = stripComments(css);
    for (const pattern of COLOR_LITERALS) {
      expect(body).not.toMatch(pattern);
    }
  });
});
