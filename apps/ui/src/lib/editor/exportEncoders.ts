import type { EncoderInfo } from './api';

/** One dropdown entry in the export encoder selector. */
export interface EncoderOption {
  value: string;
  label: string;
}

/** Display text for one encoder: its friendly `label` when present, else name plus GPU/CPU. */
export function encoderLabelFor(e: EncoderInfo): string {
  if (typeof e.label === 'string' && e.label.trim() !== '') return e.label;
  if (e.name === 'software') return 'Software (CPU)';
  return `${e.name} (${e.hardware ? 'GPU' : 'CPU'})`;
}

/**
 * Builds the encoder dropdown options.
 * - `Auto (best available)` is always first; when the first hardware entry has a label it is appended.
 * - One option per distinct `name` (first occurrence wins); the `auto` pseudo-entry is skipped.
 * - `current` is appended when it is not already in the list.
 * `list` null means the list is still loading and only software is offered.
 */
export function encoderOptionsFor(
  list: EncoderInfo[] | null,
  current: string | null | undefined,
): EncoderOption[] {
  const source: EncoderInfo[] = list ?? [{ name: 'software', hardware: false }];
  const bestHardware = source.find((e) => e.hardware && e.name !== 'auto');
  const autoLabel =
    bestHardware && typeof bestHardware.label === 'string' && bestHardware.label.trim() !== ''
      ? `Auto (best available: ${bestHardware.label})`
      : 'Auto (best available)';

  const opts: EncoderOption[] = [{ value: 'auto', label: autoLabel }];
  const seen = new Set<string>(['auto']);
  for (const e of source) {
    if (seen.has(e.name)) continue;
    seen.add(e.name);
    opts.push({ value: e.name, label: encoderLabelFor(e) });
  }

  const selected = current ?? 'auto';
  if (!seen.has(selected)) {
    opts.push({ value: selected, label: encoderLabelFor({ name: selected, hardware: false }) });
  }
  return opts;
}
