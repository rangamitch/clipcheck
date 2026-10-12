/**
 * Minimal, dependency-free Markdown parser for release notes.
 *
 * Produces a plain token tree (never HTML), so the renderer cannot inject markup.
 * Supports: headings, paragraphs, bullet/numbered lists, fenced code, horizontal rules,
 * and inline **bold**, *italic* or _italic_, `code`, ~~strike~~ and [links](url) (label only).
 */

export type Inline =
  | { t: 'text'; v: string }
  | { t: 'bold' | 'italic' | 'strike'; c: Inline[] }
  | { t: 'code'; v: string };

export type Block =
  | { t: 'heading'; level: 1 | 2 | 3; c: Inline[] }
  | { t: 'p'; c: Inline[] }
  | { t: 'list'; ordered: boolean; items: Inline[][] }
  | { t: 'code'; v: string }
  | { t: 'hr' };

const INLINE_RE =
  /`([^`\n]+)`|\*\*(.+?)\*\*|__(.+?)__|~~(.+?)~~|\*([^*\s][^*]*?)\*|(?<![A-Za-z0-9])_([^_\s][^_]*?)_(?![A-Za-z0-9])|\[([^\]\n]+)\]\(([^)\s]+)\)|<(https?:\/\/[^>\s]+)>/g;

export function parseInline(src: string): Inline[] {
  const out: Inline[] = [];
  let last = 0;
  for (const m of src.matchAll(INLINE_RE)) {
    const idx = m.index ?? 0;
    if (idx > last) out.push({ t: 'text', v: src.slice(last, idx) });
    if (m[1] !== undefined) out.push({ t: 'code', v: m[1] });
    else if (m[2] !== undefined || m[3] !== undefined) out.push({ t: 'bold', c: parseInline(m[2] ?? m[3]) });
    else if (m[4] !== undefined) out.push({ t: 'strike', c: parseInline(m[4]) });
    else if (m[5] !== undefined || m[6] !== undefined) out.push({ t: 'italic', c: parseInline(m[5] ?? m[6]) });
    else if (m[7] !== undefined) out.push(...parseInline(m[7]));
    else if (m[9] !== undefined) out.push({ t: 'text', v: m[9] });
    last = idx + m[0].length;
  }
  if (last < src.length) out.push({ t: 'text', v: src.slice(last) });
  return out;
}

export function parseMarkdown(src: string): Block[] {
  const lines = src.replace(/\r\n?/g, '\n').split('\n');
  const blocks: Block[] = [];
  let para: string[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;

  const flushPara = () => {
    if (para.length) blocks.push({ t: 'p', c: parseInline(para.join(' ')) });
    para = [];
  };
  const flushList = () => {
    if (list) blocks.push({ t: 'list', ordered: list.ordered, items: list.items.map(parseInline) });
    list = null;
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    if (/^\s*```/.test(line)) {
      flushPara();
      flushList();
      const code: string[] = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) code.push(lines[i++]);
      blocks.push({ t: 'code', v: code.join('\n') });
      continue;
    }
    if (!line.trim()) {
      flushPara();
      flushList();
      continue;
    }
    const heading = /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
    if (heading) {
      flushPara();
      flushList();
      blocks.push({
        t: 'heading',
        level: Math.min(heading[1].length, 3) as 1 | 2 | 3,
        c: parseInline(heading[2]),
      });
      continue;
    }
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) {
      flushPara();
      flushList();
      blocks.push({ t: 'hr' });
      continue;
    }
    const item = /^\s*(?:([-*+])|(\d+)[.)])\s+(.*)$/.exec(line);
    if (item) {
      flushPara();
      const ordered = item[2] !== undefined;
      if (list && list.ordered !== ordered) flushList();
      list ??= { ordered, items: [] };
      list.items.push(item[3]);
      continue;
    }
    // Indented continuation of the previous list item.
    if (list && /^\s+\S/.test(line)) {
      list.items[list.items.length - 1] += ' ' + line.trim();
      continue;
    }
    flushList();
    para.push(line.trim());
  }
  flushPara();
  flushList();
  return blocks;
}
