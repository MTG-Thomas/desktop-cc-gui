/**
 * Page-internal settings search: the index of the rows *inside* every
 * settings page, plus the pure matcher the shell runs per keystroke.
 *
 * The rail's own search only matches nav labels (page titles: 通用 / 网络代理
 * …), so a row such as 桌面宠物 inside 通用 was unfindable — typing 宠物
 * answered 没有匹配的设置 while the row sat right there. This module is the
 * declarative half of the fix (the shell renders the hits).
 *
 * The index is *declared*, never scraped: pre-rendering every page to read its
 * DOM would fire real side effects (GeneralSection reads app settings and the
 * pet list on mount, plugin pages run arbitrary code). One list per page lives
 * next to that page's section file (`general-search.ts` for 通用) and is
 * registered from `sections.tsx` where the page itself registers, so "which
 * pages are searchable" is one grep. Every entry's `anchor` must exist as a
 * `SettingsRow anchor=` on that page — `general-search.test.tsx` renders the
 * page and fails in both directions of drift (a declared row that never
 * renders, a rendered row nobody indexed).
 */

/** One searchable row. `page` + `anchor` are the coordinates the shell jumps
 *  to; the two label keys resolve at query time so language flips re-match
 *  without re-registering. */
export interface SettingsSearchEntry {
  /** Target page: the settings registry key, i.e. the `?page=` value. */
  page: string;
  /** `anchor` prop of the target row (`settings-rows.tsx`). */
  anchor: string;
  /** i18n key of the row label — the same key the row renders. */
  labelKey: string;
  /** i18n key of the card heading the row sits under; shown as the result's
   *  breadcrumb line (`通用 › 桌面宠物 › 显示桌面宠物`). */
  sectionKey: string;
  /** Extra query aliases that are not rendered anywhere (「pet」 for the pet
   *  rows, so an English habit still finds them). */
  keywords?: readonly string[];
}

/** One row a query matched, with both labels already resolved. */
export interface SettingsSearchHit {
  entry: SettingsSearchEntry;
  /** Row label as the result shows it (placeholders stripped). */
  label: string;
  /** Card heading above the row (result breadcrumb). */
  section: string;
}

const entries: SettingsSearchEntry[] = [];

/** Add one page's rows; called from `sections.tsx` next to that page's
 *  `settingsRegistry.register`. Re-registering the same (page, anchor) is a
 *  no-op, so HMR re-runs stay harmless (same contract as the section
 *  registry). */
export function registerSettingsSearchEntries(
  next: readonly SettingsSearchEntry[],
): void {
  for (const entry of next) {
    const seen = entries.some(
      (existing) =>
        existing.page === entry.page && existing.anchor === entry.anchor,
    );
    if (!seen) entries.push(entry);
  }
}

/** Every indexed row in declaration order (page order, then row order). */
export function settingsSearchEntries(): readonly SettingsSearchEntry[] {
  return entries;
}

/** i18next placeholders drop out with their parentheses — 「管理历史记录
 *  ({{count}})」 becomes 「管理历史记录」: a result row names the setting, not
 *  a runtime counter. */
function resultLabel(text: string): string {
  return text.replace(/[\s(（]*\{\{[^{}]*\}\}[)）]?/g, "").trim();
}

/**
 * Case-insensitive substring match over the row label, the card heading and
 * the aliases. Matching the card heading is deliberate: 「外观」/「桌面宠物」
 * are the names users remember, and every hit of that section then reads as a
 * path (`通用 › 桌面宠物 › 角色`) explaining why it matched. Hits keep
 * declaration order, so a page's results are in that page's own row order.
 */
export function matchSettingsSearch(
  entries: readonly SettingsSearchEntry[],
  query: string,
  translate: (key: string) => string,
): SettingsSearchHit[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [];
  const hits: SettingsSearchHit[] = [];
  for (const entry of entries) {
    const label = resultLabel(translate(entry.labelKey));
    const section = resultLabel(translate(entry.sectionKey));
    const haystack = [label, section, ...(entry.keywords ?? [])];
    if (haystack.some((text) => text.toLowerCase().includes(needle))) {
      hits.push({ entry, label, section });
    }
  }
  return hits;
}
