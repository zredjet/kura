import { describe, expect, it } from 'vitest';

import { joinArgs, splitArgs } from './RulesSheet';

describe('rule arguments', () => {
  it('splits on spaces and keeps quoted parts together', () => {
    expect(splitArgs('-f "bv*+ba" --embed-metadata')).toEqual(['-f', 'bv*+ba', '--embed-metadata']);
    expect(splitArgs(`-o 'browser=firefox' "a b"`)).toEqual(['-o', 'browser=firefox', 'a b']);
    expect(splitArgs('   ')).toEqual([]);
  });

  it('joins back so that splitting gives the same list', () => {
    const args = ['-f', 'a b', '--x', ''];
    expect(joinArgs(args)).toBe('-f "a b" --x ""');
    expect(splitArgs(joinArgs(args))).toEqual(args);
  });
});
