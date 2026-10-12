import { describe, expect, it } from 'vitest';
import { parseInline, parseMarkdown } from './markdown';

describe('markdown', () => {
  it('parses the 0.4.0 release notes shape', () => {
    const blocks = parseMarkdown(
      '# ClipCheck 0.4.0\n\n- **Experimental Capture** (Video tab): opt-in.\n- Fixes: a, b.\n',
    );
    expect(blocks[0]).toEqual({ t: 'heading', level: 1, c: [{ t: 'text', v: 'ClipCheck 0.4.0' }] });
    expect(blocks[1].t).toBe('list');
    const list = blocks[1] as Extract<(typeof blocks)[number], { t: 'list' }>;
    expect(list.items).toHaveLength(2);
    expect(list.items[0][0]).toEqual({ t: 'bold', c: [{ t: 'text', v: 'Experimental Capture' }] });
  });

  it('handles inline code, italics and links', () => {
    expect(parseInline('a `b` *c* [d](https://x.y)')).toEqual([
      { t: 'text', v: 'a ' },
      { t: 'code', v: 'b' },
      { t: 'text', v: ' ' },
      { t: 'italic', c: [{ t: 'text', v: 'c' }] },
      { t: 'text', v: ' ' },
      { t: 'text', v: 'd' },
    ]);
  });

  it('keeps html as literal text', () => {
    expect(parseInline('<script>x</script>')).toEqual([{ t: 'text', v: '<script>x</script>' }]);
  });

  it('parses ordered lists and code fences', () => {
    const blocks = parseMarkdown('1. a\n2. b\n\n```\ncode\n```');
    expect(blocks[0]).toMatchObject({ t: 'list', ordered: true });
    expect(blocks[1]).toEqual({ t: 'code', v: 'code' });
  });
});
