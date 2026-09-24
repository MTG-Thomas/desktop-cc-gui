import { IS_WINDOWS } from "@/lib/platform";
import type { SettingsSearchEntry } from "./settings-search";

/**
 * Search index for 通用 (`GeneralSection.tsx` + `PromptHistorySettings.tsx`):
 * every row of the page, in the order the page renders them, so results read
 * in the page's own row order.
 *
 * `anchor` must match the `anchor=` prop on the row (or the literal
 * `data-setting-anchor` attribute of the hand-rolled 输入历史 row, which is a
 * plain div, not a `SettingsRow`). `general-search.test.tsx` renders the page
 * and fails when either side drifts.
 */
export const generalSearchEntries: SettingsSearchEntry[] = [
  // 外观
  {
    page: "general",
    anchor: "theme",
    labelKey: "settings.theme",
    sectionKey: "settings.appearance",
  },
  // 标题栏样式只在 Windows 渲染（`IS_WINDOWS`）；其他平台不索引，避免搜索结果
  // 命中一个当前平台根本没渲染的行。
  ...(IS_WINDOWS
    ? [
        {
          page: "general",
          anchor: "titlebar",
          labelKey: "settings.titlebar",
          sectionKey: "settings.appearance",
        } satisfies SettingsSearchEntry,
      ]
    : []),
  {
    page: "general",
    anchor: "language",
    labelKey: "settings.language",
    sectionKey: "settings.appearance",
  },
  {
    page: "general",
    anchor: "sidebarThreadLimit",
    labelKey: "settings.sidebarThreadLimit",
    sectionKey: "settings.appearance",
  },
  // 桌面宠物：标签是「显示桌面宠物 / 角色 / 宠物大小」，中文查询靠卡片标题
  // （桌面宠物）命中，英文习惯由 keywords 兜底。
  {
    page: "general",
    anchor: "petEnabled",
    labelKey: "settings.petEnabled",
    sectionKey: "settings.pet",
    keywords: ["pet", "spritesheet"],
  },
  {
    page: "general",
    anchor: "petCharacter",
    labelKey: "settings.petCharacter",
    sectionKey: "settings.pet",
    keywords: ["pet"],
  },
  {
    page: "general",
    anchor: "petScale",
    labelKey: "settings.petScale",
    sectionKey: "settings.pet",
    keywords: ["pet"],
  },
  // 行为
  {
    page: "general",
    anchor: "sendShortcut",
    labelKey: "settings.sendShortcut",
    sectionKey: "settings.behavior",
  },
  {
    page: "general",
    anchor: "thinkingAutoCollapse",
    labelKey: "settings.thinkingAutoCollapse",
    sectionKey: "settings.behavior",
  },
  {
    page: "general",
    anchor: "promptHistory",
    labelKey: "settings.promptHistory",
    sectionKey: "settings.behavior",
  },
  // 输入历史：行标题带条数（管理历史记录 ({{count}})），搜索结果里去掉计数。
  {
    page: "general",
    anchor: "promptHistoryManage",
    labelKey: "settings.promptHistoryManageTitle",
    sectionKey: "settings.promptHistoryManage",
  },
];
