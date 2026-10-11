/**
 * Pure part of the NumberField contract (#186). The component commits through commitNumber,
 * so these tests pin the parse, round and clamp rules. Component behaviour (focus, Enter and
 * blur, prop sync while typing, onchange suppression) needs a DOM and is not tested here.
 */
import { describe, expect, it } from 'vitest';
import { commitNumber } from './pure';

describe('commitNumber: invalid text reverts (null)', () => {
  it.each(['abc', '', '   ', '-', '+', 'Infinity', 'NaN', '12px'])('%j gives null', (text) => {
    expect(commitNumber(text, 0, 100, true)).toBeNull();
    expect(commitNumber(text, 0, 100, false)).toBeNull();
  });
});

describe('commitNumber: parsing', () => {
  it('parses plain integers and trims whitespace', () => {
    expect(commitNumber('42', 0, 100, true)).toBe(42);
    expect(commitNumber('  42  ', 0, 100, true)).toBe(42);
  });

  it('parses negative numbers when the range allows them', () => {
    expect(commitNumber('-5', -10, 10, true)).toBe(-5);
  });

  it('parses decimals when integer-only is off', () => {
    expect(commitNumber('2.5', 0, 10, false)).toBe(2.5);
  });
});

describe('commitNumber: rounding', () => {
  it('rounds to a whole number when integer-only', () => {
    expect(commitNumber('2.4', 0, 10, true)).toBe(2);
    expect(commitNumber('2.6', 0, 10, true)).toBe(3);
  });

  it('keeps the decimal when integer-only is off', () => {
    expect(commitNumber('2.6', 0, 10, false)).toBe(2.6);
  });
});

describe('commitNumber: clamping to [min, max]', () => {
  it('clamps above max', () => {
    expect(commitNumber('150', 0, 100, true)).toBe(100);
  });

  it('clamps below min', () => {
    expect(commitNumber('-3', 0, 100, true)).toBe(0);
  });

  it('clamps a decimal above max without rounding it first when integer-only is off', () => {
    expect(commitNumber('100.5', 0, 100, false)).toBe(100);
  });

  it('rounds then clamps when integer-only: 99.6 with max 100 gives 100', () => {
    expect(commitNumber('99.6', 0, 100, true)).toBe(100);
  });

  it('a value exactly at a bound is returned unchanged', () => {
    expect(commitNumber('0', 0, 100, true)).toBe(0);
    expect(commitNumber('100', 0, 100, true)).toBe(100);
  });

  it('rounds a value that is then equal to the prop, so the component would send nothing', () => {
    // The component calls onchange only when the returned number differs from the prop (30).
    expect(commitNumber('30.2', 5, 3600, true)).toBe(30);
    expect(commitNumber('30', 5, 3600, true)).toBe(30);
  });
});
